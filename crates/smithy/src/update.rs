//! `smithy update` : remplace le binaire en cours par la dernière release GitHub.
//!
//! Même chemin que `scripts/install.sh` : archive `smithy_<os>_<arch>.tar.gz` et `checksums.txt`
//! de `releases/latest/download`, somme SHA-256 vérifiée avant d'écrire quoi que ce soit. La
//! dernière version se lit dans la redirection de `releases/latest` (pas d'API : pas de quota).
//! La signature cosign de `checksums.txt` n'est pas vérifiée ici (voir le README).

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

const REPO_URL: &str = "https://github.com/thomas-gleizes/smithy";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// `(majeure, mineure, correctif)` d'un tag `v1.2.3` ou d'une version `1.2.3[-x]`.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Nom de l'archive de cette machine, comme dans `install.sh`.
pub fn asset_name(os: &str, arch: &str) -> Result<String> {
    let os = match os {
        "linux" => "linux",
        "macos" => "darwin",
        other => bail!("système non pris en charge : {other}"),
    };
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("architecture non prise en charge : {other}"),
    };
    Ok(format!("smithy_{os}_{arch}.tar.gz"))
}

/// La somme attendue d'un fichier dans `checksums.txt` (`<sha256>  <nom>`).
pub fn expected_sum<'a>(checksums: &'a str, name: &str) -> Option<&'a str> {
    checksums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let sum = fields.next()?;
        // `sha256sum -b` préfixe le nom d'une étoile.
        let file = fields.next()?.trim_start_matches('*');
        (file == name).then_some(sum)
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Le binaire `smithy` à la racine de l'archive.
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().context("archive illisible")? {
        let mut entry = entry.context("archive illisible")?;
        if entry.path()?.as_ref() == Path::new("smithy") {
            let mut out = Vec::new();
            entry.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("pas de binaire `smithy` dans l'archive")
}

fn client(follow: bool) -> Result<reqwest::Client> {
    let policy = if follow {
        reqwest::redirect::Policy::default()
    } else {
        reqwest::redirect::Policy::none()
    };
    Ok(reqwest::Client::builder()
        .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        .redirect(policy)
        .timeout(std::time::Duration::from_secs(120))
        .build()?)
}

/// Le tag de la dernière release, lu dans la redirection de `releases/latest`.
async fn latest_tag() -> Result<String> {
    let response = client(false)?
        .get(format!("{REPO_URL}/releases/latest"))
        .send()
        .await
        .context("GitHub injoignable")?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("aucune release publiée")?;
    let tag = location
        .rsplit('/')
        .next()
        .filter(|t| parse_version(t).is_some())
        .with_context(|| format!("tag inattendu : {location}"))?;
    Ok(tag.to_string())
}

async fn download(url: &str) -> Result<Vec<u8>> {
    let response = client(true)?
        .get(url)
        .send()
        .await
        .with_context(|| format!("téléchargement de {url}"))?
        .error_for_status()
        .with_context(|| format!("téléchargement de {url}"))?;
    Ok(response.bytes().await?.to_vec())
}

/// Écrit `binary` à côté de `target` puis le renomme dessus : le remplacement est atomique et
/// reste possible pour un exécutable en cours (Unix).
fn replace_exe(target: &Path, binary: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = target.parent().context("dossier du binaire introuvable")?;
    let tmp = dir.join(format!(".smithy-update-{}", std::process::id()));
    let written = std::fs::write(&tmp, binary)
        .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| std::fs::rename(&tmp, target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "écriture refusée dans {} : relancez avec les droits nécessaires (sudo smithy update)",
                dir.display()
            );
        }
        return Err(e).with_context(|| format!("remplacement de {}", target.display()));
    }
    Ok(())
}

/// `smithy update [--check]`.
pub async fn run(check_only: bool) -> Result<()> {
    let tag = latest_tag().await?;
    let latest = parse_version(&tag).context("version illisible")?;
    let current = parse_version(CURRENT).context("version courante illisible")?;
    if latest <= current {
        println!("smithy {CURRENT} est à jour (dernière release : {tag}).");
        return Ok(());
    }
    if check_only {
        println!("Mise à jour disponible : {CURRENT} → {tag} (smithy update).");
        return Ok(());
    }
    let name = asset_name(std::env::consts::OS, std::env::consts::ARCH)?;
    let base = format!("{REPO_URL}/releases/download/{tag}");
    println!("Téléchargement de {name} ({tag})…");
    let archive = download(&format!("{base}/{name}")).await?;
    let sums = String::from_utf8(download(&format!("{base}/checksums.txt")).await?)
        .context("checksums.txt illisible")?;
    let expected =
        expected_sum(&sums, &name).with_context(|| format!("{name} absent de checksums.txt"))?;
    if !expected.eq_ignore_ascii_case(&sha256_hex(&archive)) {
        bail!("somme de contrôle incorrecte pour {name} : rien n'a été modifié");
    }
    let binary = extract_binary(&archive)?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("chemin du binaire introuvable")?;
    replace_exe(&exe, &binary)?;
    crate::log::info!("mise à jour {CURRENT} → {tag}");
    println!("smithy {CURRENT} → {tag} ({}).", exe.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0-rc1"), Some((0, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("latest"), None);
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
    }

    #[test]
    fn archives_par_machine() {
        assert_eq!(
            asset_name("macos", "aarch64").unwrap(),
            "smithy_darwin_arm64.tar.gz"
        );
        assert_eq!(
            asset_name("linux", "x86_64").unwrap(),
            "smithy_linux_amd64.tar.gz"
        );
        assert!(asset_name("windows", "x86_64").is_err());
        assert!(asset_name("linux", "riscv64").is_err());
    }

    #[test]
    fn somme_attendue() {
        let text = "aaa  smithy_linux_amd64.tar.gz\nbbb *smithy_darwin_arm64.tar.gz\n";
        assert_eq!(expected_sum(text, "smithy_linux_amd64.tar.gz"), Some("aaa"));
        assert_eq!(
            expected_sum(text, "smithy_darwin_arm64.tar.gz"),
            Some("bbb")
        );
        assert_eq!(expected_sum(text, "autre.tar.gz"), None);
    }

    #[test]
    fn sha256_connu() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn archive_with(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extrait_le_binaire() {
        let archive = archive_with(&[("README.md", b"doc"), ("smithy", b"ELF")]);
        assert_eq!(extract_binary(&archive).unwrap(), b"ELF");
        let sans = archive_with(&[("README.md", b"doc")]);
        assert!(extract_binary(&sans).is_err());
    }

    #[test]
    fn remplace_le_binaire() {
        let dir = std::env::temp_dir().join(format!("smithy-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("smithy");
        std::fs::write(&target, b"vieux").unwrap();
        replace_exe(&target, b"neuf").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"neuf");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
