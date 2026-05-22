use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use hudsucker::rustls::crypto::aws_lc_rs;

pub(crate) fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve home directory"))?;
    Ok(home.join(".id4pii"))
}

fn ca_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("ca"))
}

pub(crate) fn cert_path() -> Result<PathBuf> {
    Ok(ca_dir()?.join("id4pii-ca.crt"))
}

fn key_path() -> Result<PathBuf> {
    Ok(ca_dir()?.join("id4pii-ca.key"))
}

fn load_or_create() -> Result<(String, String)> {
    let cert_file = cert_path()?;
    let key_file = key_path()?;
    if cert_file.exists() && key_file.exists() {
        let cert = std::fs::read_to_string(&cert_file)?;
        let key = std::fs::read_to_string(&key_file)?;
        return Ok((cert, key));
    }
    let (cert, key) = generate()?;
    std::fs::create_dir_all(ca_dir()?)?;
    std::fs::write(&cert_file, &cert)?;
    std::fs::write(&key_file, &key)?;
    Ok((cert, key))
}

fn generate() -> Result<(String, String)> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "id4pii local CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "id4pii");
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

pub(crate) fn ensure() -> Result<PathBuf> {
    load_or_create()?;
    cert_path()
}

pub(crate) fn authority() -> Result<RcgenAuthority> {
    let (cert_pem, key_pem) = load_or_create()?;
    let key_pair = KeyPair::from_pem(&key_pem).context("invalid CA key")?;
    let issuer = Issuer::from_ca_cert_pem(&cert_pem, key_pair).context("invalid CA certificate")?;
    Ok(RcgenAuthority::new(
        issuer,
        1_000,
        aws_lc_rs::default_provider(),
    ))
}

pub(crate) fn install() -> Result<()> {
    let path = cert_path()?;
    load_or_create()?;
    #[cfg(windows)]
    {
        let status = std::process::Command::new("certutil")
            .args(["-addstore", "-user", "Root"])
            .arg(&path)
            .status()
            .context("failed to run certutil")?;
        if !status.success() {
            return Err(anyhow!(
                "certutil failed; install {} into the current-user Root store manually",
                path.display()
            ));
        }
        println!("id4pii CA installed into the current-user Root store.");
    }
    #[cfg(not(windows))]
    {
        println!(
            "Install this CA into your OS trust store manually: {}",
            path.display()
        );
    }
    println!(
        "Firefox uses its own store: import {} via Settings > Privacy > View Certificates > Authorities.",
        path.display()
    );
    Ok(())
}

pub(crate) fn export(out: Option<&Path>) -> Result<()> {
    let (cert, _) = load_or_create()?;
    match out {
        Some(path) => {
            std::fs::write(path, cert)?;
            println!("CA certificate written to {}", path.display());
        }
        None => print!("{cert}"),
    }
    Ok(())
}
