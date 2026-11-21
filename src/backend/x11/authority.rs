use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use x11rb_protocol::xauth::{Family, get_auth};

const MIT_COOKIE: &[u8] = b"MIT-MAGIC-COOKIE-1";

#[derive(Debug, Default, PartialEq, Eq)]
pub enum X11Authority {
    #[default]
    Environment,
    File(PathBuf),
}

pub(super) enum DefaultLookup {
    BestEffort,
    #[cfg(unix)]
    CheckFile,
}

#[derive(Default)]
pub(super) struct Authorization {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

impl X11Authority {
    pub(super) fn lookup(
        &self,
        family: Family,
        address: &[u8],
        display: u16,
        default: DefaultLookup,
    ) -> Result<Authorization> {
        if let Self::File(path) = self {
            return load_file(
                path,
                AuthQuery {
                    family,
                    address,
                    display,
                },
            );
        }

        let auth = match (default, get_auth(family, address, display)) {
            (_, Ok(auth)) => auth,
            (DefaultLookup::BestEffort, Err(_)) => None,
            #[cfg(unix)]
            (DefaultLookup::CheckFile, Err(error))
                if error.kind() == io::ErrorKind::NotFound
                    && std::env::var_os("XAUTHORITY").is_none() =>
            {
                None
            }
            #[cfg(unix)]
            (DefaultLookup::CheckFile, Err(error)) => {
                return Err(error).context("could not read X11 authorization");
            }
        };
        let (name, data) = auth.unwrap_or_default();
        Ok(Authorization { name, data })
    }
}

struct AuthQuery<'a> {
    family: Family,
    address: &'a [u8],
    display: u16,
}

fn load_file(path: &Path, query: AuthQuery<'_>) -> Result<Authorization> {
    let file = File::open(path)
        .with_context(|| format!("could not open Xauthority file {}", path.display()))?;
    let display = query.display;
    find_cookie(BufReader::new(file), query)
        .with_context(|| format!("could not read Xauthority file {}", path.display()))?
        .with_context(|| {
            format!(
                "no matching X11 cookie for display {display} in {}",
                path.display()
            )
        })
}

// The x11rb helper only reads the process environment. For an explicit file,
// read the same length-prefixed records and apply the same matching rules.
fn find_cookie(
    mut reader: impl BufRead,
    query: AuthQuery<'_>,
) -> io::Result<Option<Authorization>> {
    let display = query.display.to_string();
    while !reader.fill_buf()?.is_empty() {
        let family = Family::from(read_u16(&mut reader)?);
        let address = read_field(&mut reader)?;
        let number = read_field(&mut reader)?;
        let name = read_field(&mut reader)?;
        let data = read_field(&mut reader)?;
        let peer_matches = family == Family::WILD
            || query.family == Family::WILD
            || (family == query.family && address == query.address);
        if peer_matches && (number.is_empty() || number == display.as_bytes()) && name == MIT_COOKIE
        {
            return Ok(Some(Authorization { name, data }));
        }
    }
    Ok(None)
}

fn read_u16(reader: &mut impl Read) -> io::Result<u16> {
    let mut bytes = [0; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_field(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; usize::from(read_u16(reader)?)];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(family: u16, fields: [&[u8]; 4]) -> Vec<u8> {
        let mut record = family.to_be_bytes().to_vec();
        for field in fields {
            record.extend_from_slice(&(field.len() as u16).to_be_bytes());
            record.extend_from_slice(field);
        }
        record
    }

    fn local_display() -> AuthQuery<'static> {
        AuthQuery {
            family: Family::LOCAL,
            address: b"host",
            display: 0,
        }
    }

    #[test]
    fn finds_a_matching_cookie_and_skips_other_records() -> io::Result<()> {
        let mut file = record(256, [b"other-host", b"0", MIT_COOKIE, b"wrong host"]);
        file.extend(record(65535, [b"any", b"1", MIT_COOKIE, b"wrong display"]));
        file.extend(record(256, [b"host", b"0", b"OTHER", b"wrong mechanism"]));
        file.extend(record(256, [b"host", b"0", MIT_COOKIE, b"matched"]));
        let auth = find_cookie(file.as_slice(), local_display())?.unwrap();
        assert_eq!(auth.name, MIT_COOKIE);
        assert_eq!(auth.data, b"matched");
        Ok(())
    }

    #[test]
    fn accepts_wildcard_family_and_empty_display() -> io::Result<()> {
        let file = record(65535, [b"container", b"", MIT_COOKIE, b"matched"]);
        let auth = find_cookie(file.as_slice(), local_display())?.unwrap();
        assert_eq!(auth.data, b"matched");
        Ok(())
    }

    #[test]
    fn distinguishes_missing_entries_from_truncated_files() {
        assert!(find_cookie(&[][..], local_display()).unwrap().is_none());
        let file = record(65535, [b"any", b"1", MIT_COOKIE, b"wrong display"]);
        assert!(
            find_cookie(file.as_slice(), local_display())
                .unwrap()
                .is_none()
        );
        assert!(find_cookie(&file[..file.len() - 1], local_display()).is_err());
    }
}
