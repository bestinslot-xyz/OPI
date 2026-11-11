//! Bitcoin addresses and pkScripts as the inscriptions and runes modules
//! store them: the pkScript as raw bytes, and the address derived from it
//! for the five script types with an address form (P2PKH, P2SH, P2WPKH,
//! P2WSH, P2TR) in the encoding of the database's network (`bc`; `tb` for
//! testnet, testnet4 and signet; `bcrt`).
//!
//! An `address` parameter resolves to the pkScript it encodes, so a route
//! filters on the pkScript alone. An address that does not parse, belongs
//! to another network or encodes another script type resolves to no
//! pkScript, and the route answers no rows for it.

use std::str::FromStr;

use bitcoin::{Address, Network, address::NetworkUnchecked};

use crate::{db::Db, reply::ApiError, routes::paging::Wallet};

/// The network table of the module a route reads.
#[derive(Debug, Clone, Copy)]
pub enum NetworkTable {
    Inscriptions,
    Runes,
}

impl NetworkTable {
    fn query(self) -> &'static str {
        match self {
            NetworkTable::Inscriptions => {
                "select network_type from inscriptions_network_type limit 1"
            }
            NetworkTable::Runes => "select network_type from runes_network_type limit 1",
        }
    }
}

/// The network the module's database was indexed for, as `NETWORK_TYPE`
/// names it.
pub async fn network_type(db: &Db, table: NetworkTable) -> Result<String, ApiError> {
    let network: Option<String> = sqlx::query_scalar(table.query())
        .fetch_optional(db.pool()?)
        .await?;
    Ok(network.unwrap_or_default())
}

fn network(network_type: &str) -> Option<Network> {
    match network_type {
        "mainnet" => Some(Network::Bitcoin),
        "testnet" | "testnet4" | "signet" => Some(Network::Testnet),
        "regtest" => Some(Network::Regtest),
        _ => None,
    }
}

/// The address of `pkscript` on `network_type`, `None` for a script
/// without an address form.
pub fn address_of(pkscript: &[u8], network_type: &str) -> Option<String> {
    opi_inscriptions::address(pkscript, network_type)
}

/// The pkScript `address` encodes on `network_type`, `None` when it does
/// not parse, is for another network or encodes a script type without a
/// stored address form.
pub fn pkscript_of(address: &str, network_type: &str) -> Option<Vec<u8>> {
    let network = network(network_type)?;
    let parsed = Address::<NetworkUnchecked>::from_str(address).ok()?;
    if !parsed.is_valid_for_network(network) {
        return None;
    }
    let script = parsed.assume_checked().script_pubkey().into_bytes();
    address_of(&script, network_type).map(|_| script)
}

impl Wallet {
    /// The wallet's pkScript bytes on `network_type`, `None` for an address
    /// that resolves to none.
    pub fn pkscript_bytes(&self, network_type: &str) -> Option<Vec<u8>> {
        match self {
            Wallet::Address(address) => pkscript_of(address, network_type),
            Wallet::PkScript(hex) => hex::decode(hex).ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_resolves_to_the_pkscript_it_encodes() {
        let p2wpkh = hex::decode("0014751e76e8199196d454941c45d1b3a323f1433bd6").unwrap();
        let address = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        assert_eq!(address_of(&p2wpkh, "mainnet").as_deref(), Some(address));
        assert_eq!(pkscript_of(address, "mainnet"), Some(p2wpkh.clone()));
        assert_eq!(
            pkscript_of(&address.to_ascii_uppercase(), "mainnet"),
            Some(p2wpkh)
        );
    }

    #[test]
    fn an_address_of_another_network_or_type_resolves_to_none() {
        let address = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        assert_eq!(pkscript_of(address, "signet"), None);
        assert_eq!(pkscript_of("addrA", "mainnet"), None);
        assert_eq!(pkscript_of(address, "nonet"), None);
        // witness v2: valid, but no stored address form
        let v2 = "bc1zw508d6qejxtdg4y5r3zarvaryvaxxpcs";
        assert_eq!(pkscript_of(v2, "mainnet"), None);
    }

    #[test]
    fn signet_and_regtest_take_their_own_encodings() {
        let p2wpkh = hex::decode(format!("0014{}", "11".repeat(20))).unwrap();
        for network in ["signet", "regtest"] {
            let address = address_of(&p2wpkh, network).unwrap();
            assert_eq!(pkscript_of(&address, network), Some(p2wpkh.clone()));
        }
    }
}
