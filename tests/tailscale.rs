//! `vigie tailscale` against the real Tailscale CLI, in a scratch state directory.

use serde_json::{Value, json};

#[test]
#[ignore = "needs Tailscale logged in, with HTTPS certificates on"]
fn set_up_issues_a_certificate_and_rewrites_the_config() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.json");
    std::fs::write(
        &config_path,
        json!({
            "bind": "127.0.0.1:9000",
            "self_signed": true,
            "allow_origins": ["https://other.example"],
        })
        .to_string(),
    )
    .unwrap();

    let setup = vigie::tailscale::set_up(tmp.path()).unwrap();

    let cert = setup.cert_dir.join(format!("{}.crt", setup.domain));
    let key = setup.cert_dir.join(format!("{}.key", setup.domain));
    let pem = std::fs::read(&cert).unwrap();
    let (_, parsed) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
    let parsed = parsed.parse_x509().unwrap();
    assert!(parsed.validity().is_valid());
    assert!(key.is_file());

    let config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(
        config,
        json!({
            "bind": "0.0.0.0:9000",
            "cert": cert,
            "key": key,
            "public_hosts": [setup.domain],
            "allow_origins": ["https://other.example"],
        })
    );
    assert_eq!(setup.bind, "0.0.0.0:9000".parse().unwrap());
}
