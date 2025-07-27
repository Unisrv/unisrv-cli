use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Port {
    Tcp(u16),
    Udp(u16),
}

impl Serialize for Port {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Port::Tcp(port) => serializer.serialize_str(&format!("tcp:{}", port)),
            Port::Udp(port) => serializer.serialize_str(&format!("udp:{}", port)),
        }
    }
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PortVisitor;

        impl<'de> Visitor<'de> for PortVisitor {
            type Value = Port;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a string with protocol:port or just a number where tcp will be assumed",
                )
            }

            fn visit_u16<E>(self, v: u16) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Port::Tcp(v))
            }

            fn visit_str<E>(self, value: &str) -> Result<Port, E>
            where
                E: de::Error,
            {
                if let Ok(port) = value.parse::<u16>() {
                    return Ok(Port::Tcp(port));
                }

                let parts: Vec<&str> = value.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let proto = parts[0].to_ascii_lowercase();
                    let port_str = parts[1];
                    let port = port_str.parse::<u16>().map_err(de::Error::custom)?;
                    match proto.as_str() {
                        "tcp" => Ok(Port::Tcp(port)),
                        "udp" => Ok(Port::Udp(port)),
                        _ => Err(de::Error::custom("protocol must be 'tcp' or 'udp'")),
                    }
                } else {
                    Err(de::Error::custom("invalid port format"))
                }
            }
        }

        deserializer.deserialize_any(PortVisitor)
    }
}
