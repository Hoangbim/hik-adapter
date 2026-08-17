#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeError;

pub fn parse_single_range(
    header: Option<&str>,
    file_len: u64,
) -> Result<Option<ByteRange>, RangeError> {
    let Some(header) = header else {
        return Ok(None);
    };
    if file_len == 0 || !header.starts_with("bytes=") || header.contains(',') {
        return Err(RangeError);
    }
    let range = &header[6..];
    let (start, end) = range.split_once('-').ok_or(RangeError)?;
    let value = if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| RangeError)?;
        if suffix == 0 {
            return Err(RangeError);
        }
        ByteRange {
            start: file_len.saturating_sub(suffix.min(file_len)),
            end: file_len - 1,
        }
    } else {
        let start: u64 = start.parse().map_err(|_| RangeError)?;
        if start >= file_len {
            return Err(RangeError);
        }
        let end = if end.is_empty() {
            file_len - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| RangeError)?
                .min(file_len - 1)
        };
        if end < start {
            return Err(RangeError);
        }
        ByteRange { start, end }
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_single_range(None, 100).unwrap(), None);
        assert_eq!(
            parse_single_range(Some("bytes=10-19"), 100).unwrap(),
            Some(ByteRange { start: 10, end: 19 })
        );
        assert_eq!(
            parse_single_range(Some("bytes=90-"), 100).unwrap(),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_single_range(Some("bytes=-10"), 100).unwrap(),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert!(parse_single_range(Some("bytes=0-1,3-4"), 100).is_err());
    }
}
