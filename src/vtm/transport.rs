use crate::error::AdapterError;

#[derive(Default)]
pub struct TransportLock {
    locked: bool,
}

impl TransportLock {
    pub fn accept<'a>(&mut self, body: &'a [u8]) -> Result<Option<&'a [u8]>, AdapterError> {
        if self.locked {
            return Ok(Some(body));
        }
        let marker = [0x00, 0x00, 0x01, 0xba];
        let Some(offset) = body
            .windows(marker.len())
            .position(|window| window == marker)
        else {
            return Ok(None);
        };
        self.locked = true;
        Ok(Some(&body[offset..]))
    }
}
