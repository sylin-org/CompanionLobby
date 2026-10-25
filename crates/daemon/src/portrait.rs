//! Companion portraits.
//!
//! A portrait is an image the sponsor uploads on the manager page. The state
//! file records only which kind of image a companion's portrait is; the
//! image itself is kept beside it as `<companion id>.portrait`, written
//! atomically like the state file, and is not sealed: it is no secret. Only
//! PNG, JPEG, WebP and GIF images are kept, recognized by their own leading
//! bytes, never by what the browser claimed; a vector image, which can carry
//! script, is never kept. A companion without a portrait shows its initials.

use std::io;
use std::path::Path;

use lobby_contracts::CompanionId;

use crate::store::atomic_write;

/// The largest portrait kept, in bytes.
pub const MAX_BYTES: usize = 512 * 1024;
/// The suffix of a kept portrait's file name.
pub const SUFFIX: &str = ".portrait";

/// Which kind of image a portrait is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Portrait {
    /// A PNG image.
    Png,
    /// A JPEG image.
    Jpeg,
    /// A WebP image.
    Webp,
    /// A GIF image.
    Gif,
}

impl Portrait {
    /// Recognizes an image by its leading bytes.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    /// The name the state file records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Gif => "gif",
        }
    }

    /// The kind the state file names `text`, if any.
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Png, Self::Jpeg, Self::Webp, Self::Gif]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }

    /// The media type it is served with.
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

/// The file name of `companion`'s portrait.
pub fn file(companion: CompanionId) -> String {
    format!("{companion}{SUFFIX}")
}

/// Keeps `bytes` as `companion`'s portrait, replacing any before it.
pub fn write(dir: &Path, companion: CompanionId, bytes: &[u8]) -> io::Result<()> {
    atomic_write(&dir.join(file(companion)), bytes)
}

/// Reads `companion`'s portrait.
pub fn read(dir: &Path, companion: CompanionId) -> io::Result<Vec<u8>> {
    std::fs::read(dir.join(file(companion)))
}

/// Deletes `companion`'s portrait; one already gone is not an error.
pub fn remove(dir: &Path, companion: CompanionId) -> io::Result<()> {
    match std::fs::remove_file(dir.join(file(companion))) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Decodes the image of a `data:` URL the page read the chosen file into:
/// `data:<anything>;base64,<standard base64>`. What the URL claims the
/// image is counts for nothing; [`Portrait::sniff`] decides.
pub fn from_data_url(url: &str) -> Option<Vec<u8>> {
    let (head, data) = url.strip_prefix("data:")?.split_once(',')?;
    if !head.ends_with(";base64") || data.len() > (MAX_BYTES / 3 + 1) * 4 {
        return None;
    }
    decode_base64(data)
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let trimmed = text.trim_end_matches('=');
    if !text.len().is_multiple_of(4) || text.len() - trimmed.len() > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    for chunk in trimmed.as_bytes().chunks(4) {
        let mut bits = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            bits |= value(*byte)? << (18 - 6 * index);
        }
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognized_by_their_own_bytes() {
        assert_eq!(
            Portrait::sniff(b"\x89PNG\r\n\x1a\nrest"),
            Some(Portrait::Png)
        );
        assert_eq!(
            Portrait::sniff(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(Portrait::Jpeg)
        );
        assert_eq!(
            Portrait::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(Portrait::Webp)
        );
        assert_eq!(Portrait::sniff(b"GIF89a"), Some(Portrait::Gif));
        for refused in [&b"<svg xmlns"[..], b"", b"RIFF\0\0\0\0WAVE", b"%PDF-1.7"] {
            assert_eq!(Portrait::sniff(refused), None);
        }
        for kind in [Portrait::Png, Portrait::Jpeg, Portrait::Webp, Portrait::Gif] {
            assert_eq!(Portrait::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn a_data_url_decodes_whatever_it_claims() {
        assert_eq!(
            from_data_url("data:image/png;base64,iVBORw0KGgo=").as_deref(),
            Some(&b"\x89PNG\r\n\x1a\n"[..])
        );
        assert_eq!(
            from_data_url("data:text/plain;base64,TWE=").as_deref(),
            Some(&b"Ma"[..])
        );
        assert_eq!(
            from_data_url("data:;base64,TWFu").as_deref(),
            Some(&b"Man"[..])
        );
        for refused in [
            "image/png;base64,TWFu",
            "data:image/png,TWFu",
            "data:image/png;base64,TWF",
            "data:image/png;base64,TW=u",
            "data:image/png;base64,T===",
            "data:image/png;base64,TW!u",
        ] {
            assert_eq!(from_data_url(refused), None, "{refused}");
        }
        let large = format!(
            "data:image/png;base64,{}",
            "A".repeat((MAX_BYTES / 3 + 2) * 4)
        );
        assert_eq!(from_data_url(&large), None);
    }
}
