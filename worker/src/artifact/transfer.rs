use super::Error;

/// Aggregate serialized bytes per direction, owned by the WIT capability contract.
pub const BUFFER_LIMIT: usize = 1_048_576;

/// Replace a transfer destination only after checking the complete input.
pub fn copy(source: &[u8], destination: &mut Vec<u8>) -> Result<(), Error> {
    if source.len() > BUFFER_LIMIT {
        return Err(Error::new("buffer_limit_exceeded"));
    }
    destination.clear();
    destination.extend_from_slice(source);
    Ok(())
}
