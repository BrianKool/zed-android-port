//! Small, dependency-free ELF classifier used before launching user binaries.
//!
//! Android's `execve` reports a missing dynamic loader as `ENOENT`, which is
//! indistinguishable from a missing executable at the call site. Reading
//! `PT_INTERP` first lets Zdroid explain whether a binary needs Android's
//! Bionic linker, musl, glibc, or no loader at all.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_AARCH64: u16 = 183;
const PT_INTERP: u32 = 3;
const MAX_INTERPRETER_BYTES: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    Aarch64,
    Other(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibcFamily {
    Bionic,
    Musl,
    Glibc,
    Unknown,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryFormat {
    Elf,
    Script { interpreter: Option<String> },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfInfo {
    pub format: BinaryFormat,
    pub architecture: Option<Architecture>,
    pub interpreter: Option<String>,
    pub libc: LibcFamily,
    pub dynamically_linked: bool,
}

impl ElfInfo {
    pub fn is_native_arm64(&self) -> bool {
        self.architecture == Some(Architecture::Aarch64)
    }

    pub fn compatibility_summary(&self) -> String {
        match &self.format {
            BinaryFormat::Script { interpreter } => interpreter
                .as_ref()
                .map(|value| format!("script using {value}"))
                .unwrap_or_else(|| "script with an invalid shebang".into()),
            BinaryFormat::Other => "non-ELF file".into(),
            BinaryFormat::Elf => {
                let architecture = match self.architecture {
                    Some(Architecture::Aarch64) => "ARM64".to_string(),
                    Some(Architecture::Other(machine)) => format!("ELF machine {machine}"),
                    None => "unknown architecture".into(),
                };
                let runtime = match self.libc {
                    LibcFamily::Bionic => "Android/Bionic",
                    LibcFamily::Musl => "Linux/musl",
                    LibcFamily::Glibc => "Linux/glibc",
                    LibcFamily::Unknown => "unknown dynamic loader",
                    LibcFamily::None => "static",
                };
                format!("{architecture} {runtime}")
            }
        }
    }
}

pub fn inspect_binary(path: &Path) -> Result<ElfInfo> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut ident = [0_u8; 64];
    let bytes_read = file
        .read(&mut ident)
        .with_context(|| format!("read header from {}", path.display()))?;

    if bytes_read >= 2 && &ident[..2] == b"#!" {
        let line_end = ident[..bytes_read]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(bytes_read);
        let interpreter = std::str::from_utf8(&ident[2..line_end])
            .ok()
            .and_then(|line| line.split_whitespace().next())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        return Ok(ElfInfo {
            format: BinaryFormat::Script { interpreter },
            architecture: None,
            interpreter: None,
            libc: LibcFamily::Unknown,
            dynamically_linked: false,
        });
    }

    if bytes_read < ELF_MAGIC.len() || &ident[..4] != ELF_MAGIC {
        return Ok(ElfInfo {
            format: BinaryFormat::Other,
            architecture: None,
            interpreter: None,
            libc: LibcFamily::Unknown,
            dynamically_linked: false,
        });
    }
    if bytes_read < 64 {
        bail!("{} has a truncated ELF header", path.display());
    }
    if ident[4] != ELFCLASS64 {
        bail!("{} is not a 64-bit ELF binary", path.display());
    }
    if ident[5] != ELFDATA2LSB {
        bail!("{} is not a little-endian ELF binary", path.display());
    }

    let machine = read_u16(&ident, 18)?;
    let architecture = if machine == EM_AARCH64 {
        Architecture::Aarch64
    } else {
        Architecture::Other(machine)
    };
    let program_headers_offset = read_u64(&ident, 32)?;
    let program_header_size = u64::from(read_u16(&ident, 54)?);
    let program_header_count = u64::from(read_u16(&ident, 56)?);
    if program_header_size < 56 && program_header_count > 0 {
        bail!("{} has an invalid ELF program-header size", path.display());
    }

    let file_len = file.metadata()?.len();
    let table_size = program_header_size
        .checked_mul(program_header_count)
        .context("ELF program-header table size overflow")?;
    let table_end = program_headers_offset
        .checked_add(table_size)
        .context("ELF program-header table offset overflow")?;
    if table_end > file_len {
        bail!(
            "{} has a truncated ELF program-header table",
            path.display()
        );
    }

    let mut interpreter = None;
    let mut header = [0_u8; 56];
    for index in 0..program_header_count {
        let offset = program_headers_offset + index * program_header_size;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)?;
        if read_u32(&header, 0)? != PT_INTERP {
            continue;
        }
        let string_offset = read_u64(&header, 8)?;
        let string_size = read_u64(&header, 32)?;
        if string_size == 0 || string_size > MAX_INTERPRETER_BYTES {
            bail!("{} has an invalid PT_INTERP size", path.display());
        }
        let string_end = string_offset
            .checked_add(string_size)
            .context("ELF interpreter offset overflow")?;
        if string_end > file_len {
            bail!("{} has a truncated PT_INTERP value", path.display());
        }
        let mut bytes = vec![0; string_size as usize];
        file.seek(SeekFrom::Start(string_offset))?;
        file.read_exact(&mut bytes)?;
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        interpreter = Some(
            String::from_utf8(bytes)
                .with_context(|| format!("{} has a non-UTF-8 PT_INTERP", path.display()))?,
        );
        break;
    }

    let libc = interpreter
        .as_deref()
        .map(classify_interpreter)
        .unwrap_or(LibcFamily::None);
    Ok(ElfInfo {
        format: BinaryFormat::Elf,
        architecture: Some(architecture),
        dynamically_linked: interpreter.is_some(),
        interpreter,
        libc,
    })
}

fn classify_interpreter(interpreter: &str) -> LibcFamily {
    if interpreter.contains("/linker") {
        LibcFamily::Bionic
    } else if interpreter.contains("ld-musl-") {
        LibcFamily::Musl
    } else if interpreter.contains("ld-linux-") || interpreter.contains("ld-linux.") {
        LibcFamily::Glibc
    } else {
        LibcFamily::Unknown
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .context("truncated ELF u16 field")?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .context("truncated ELF u32 field")?;
    Ok(u32::from_le_bytes(value.try_into().expect("four bytes")))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .context("truncated ELF u64 field")?;
    Ok(u64::from_le_bytes(value.try_into().expect("eight bytes")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

    fn synthetic_elf(interpreter: Option<&str>, machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 256];
        bytes[..4].copy_from_slice(ELF_MAGIC);
        bytes[4] = ELFCLASS64;
        bytes[5] = ELFDATA2LSB;
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&(u16::from(interpreter.is_some())).to_le_bytes());
        if let Some(interpreter) = interpreter {
            let mut value = interpreter.as_bytes().to_vec();
            value.push(0);
            bytes[64..68].copy_from_slice(&PT_INTERP.to_le_bytes());
            bytes[72..80].copy_from_slice(&128_u64.to_le_bytes());
            bytes[96..104].copy_from_slice(&(value.len() as u64).to_le_bytes());
            bytes[128..128 + value.len()].copy_from_slice(&value);
        }
        bytes
    }

    fn inspect(bytes: &[u8]) -> ElfInfo {
        let path = std::env::temp_dir().join(format!(
            "zdroid-elf-test-{}-{}",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        drop(file);
        let result = inspect_binary(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        result
    }

    #[test]
    fn classifies_android_bionic() {
        let info = inspect(&synthetic_elf(Some("/system/bin/linker64"), EM_AARCH64));
        assert_eq!(info.architecture, Some(Architecture::Aarch64));
        assert_eq!(info.libc, LibcFamily::Bionic);
    }

    #[test]
    fn classifies_glibc_and_musl() {
        let glibc = inspect(&synthetic_elf(
            Some("/lib/ld-linux-aarch64.so.1"),
            EM_AARCH64,
        ));
        assert_eq!(glibc.libc, LibcFamily::Glibc);
        let musl = inspect(&synthetic_elf(
            Some("/lib/ld-musl-aarch64.so.1"),
            EM_AARCH64,
        ));
        assert_eq!(musl.libc, LibcFamily::Musl);
    }

    #[test]
    fn classifies_static_and_wrong_architecture() {
        let info = inspect(&synthetic_elf(None, 62));
        assert_eq!(info.libc, LibcFamily::None);
        assert_eq!(info.architecture, Some(Architecture::Other(62)));
    }

    #[test]
    fn recognizes_script_interpreter() {
        let info = inspect(b"#!/usr/bin/env node\nconsole.log('ok')\n");
        assert_eq!(
            info.format,
            BinaryFormat::Script {
                interpreter: Some("/usr/bin/env".into())
            }
        );
    }
}
