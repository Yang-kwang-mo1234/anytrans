//! The device's long-lived secret key. Its public half is the device id.

use std::{fs, io, path::Path};

use anyhow::{anyhow, Context, Result};
use data_encoding::HEXLOWER;
use iroh::SecretKey;

const KEY_FILE: &str = "device.key";

pub(crate) fn load_or_create(data_dir: &Path) -> Result<SecretKey> {
    let path = data_dir.join(KEY_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let bytes = HEXLOWER
                .decode(text.trim().as_bytes())
                .with_context(|| format!("{} is not valid hex", path.display()))?;
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("{} has the wrong length", path.display()))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            write_private(&path, HEXLOWER.encode(&key.to_bytes()).as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(data)
}
