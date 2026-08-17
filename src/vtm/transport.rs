use crate::error::{AdapterError, ErrorCategory};

#[derive(Default)]
pub struct TransportLock {
    locked: bool,
}

impl TransportLock {
    pub fn accept(&mut self, body: &[u8]) -> Result<(), AdapterError> {
        if !self.locked {
            if !body.starts_with(&[0x00, 0x00, 0x01, 0xba]) {
                return Err(AdapterError::new(
                    ErrorCategory::UnsupportedTransport,
                    "first media packet is not MPEG-PS",
                ));
            }
            self.locked = true;
        }
        Ok(())
    }
}
