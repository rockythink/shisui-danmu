use brotli::Decompressor;
use flate2::read::ZlibDecoder;
use std::io::Read;
use thiserror::Error;

const HEADER_LENGTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub version: u16,
    pub operation: u32,
    pub sequence: u32,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum PacketError {
    #[error("B 站数据包不足 16 字节")]
    ShortHeader,
    #[error("B 站数据包长度无效")]
    InvalidLength,
    #[error("B 站压缩数据解码失败: {0}")]
    Decompression(String),
}

pub fn encode_packet(operation: u32, body: &[u8]) -> Vec<u8> {
    let total = HEADER_LENGTH + body.len();
    let mut packet = Vec::with_capacity(total);
    packet.extend_from_slice(&(total as u32).to_be_bytes());
    packet.extend_from_slice(&(HEADER_LENGTH as u16).to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.extend_from_slice(&operation.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

pub fn parse_packets(data: &[u8]) -> Result<Vec<Packet>, PacketError> {
    let mut output = Vec::new();
    parse_into(data, &mut output)?;
    Ok(output)
}

fn parse_into(mut data: &[u8], output: &mut Vec<Packet>) -> Result<(), PacketError> {
    while !data.is_empty() {
        if data.len() < HEADER_LENGTH {
            return Err(PacketError::ShortHeader);
        }
        let total = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let header = u16::from_be_bytes(data[4..6].try_into().unwrap()) as usize;
        if total < header || header < HEADER_LENGTH || total > data.len() {
            return Err(PacketError::InvalidLength);
        }
        let version = u16::from_be_bytes(data[6..8].try_into().unwrap());
        let operation = u32::from_be_bytes(data[8..12].try_into().unwrap());
        let sequence = u32::from_be_bytes(data[12..16].try_into().unwrap());
        let body = &data[header..total];

        match (operation, version) {
            (5, 2) => {
                let mut decoder = ZlibDecoder::new(body);
                let mut decoded = Vec::new();
                decoder
                    .read_to_end(&mut decoded)
                    .map_err(|error| PacketError::Decompression(error.to_string()))?;
                parse_into(&decoded, output)?;
            }
            (5, 3) => {
                let mut decoder = Decompressor::new(body, 4096);
                let mut decoded = Vec::new();
                decoder
                    .read_to_end(&mut decoded)
                    .map_err(|error| PacketError::Decompression(error.to_string()))?;
                parse_into(&decoded, output)?;
            }
            _ => output.push(Packet {
                version,
                operation,
                sequence,
                body: body.to_vec(),
            }),
        }
        data = &data[total..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_plain_packet() {
        let bytes = encode_packet(7, br#"{"roomid":1}"#);
        let packets = parse_packets(&bytes).unwrap();
        assert_eq!(packets[0].operation, 7);
        assert_eq!(packets[0].body, br#"{"roomid":1}"#);
    }

    #[test]
    fn rejects_truncated_packet() {
        assert!(matches!(
            parse_packets(&[0; 4]),
            Err(PacketError::ShortHeader)
        ));
    }
}
