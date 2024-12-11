const bitcoin = require('bitcoinjs-lib');
const ecc = require('tiny-secp256k1');
bitcoin.initEccLib(ecc)

let network = bitcoin.networks.bitcoin;
if (process.env.NETWORK == "testnet") {
    network = bitcoin.networks.testnet;
} else if (process.env.NETWORK == "regtest") {
    network = bitcoin.networks.regtest;
}

const pkscript_to_address = {};
function address_from_pkscript(pkscript) {
    if (pkscript_to_address[pkscript]) {
        return pkscript_to_address[pkscript];
    }
    let ret = null;
    try {
        let address = bitcoin.payments.p2tr({
            output: Buffer.from(pkscript, "hex"),
            network: network
        });
        ret = address.address;
    } catch {
        /* try others */
    }
    try {
        let address = bitcoin.payments.p2wsh({
            output: Buffer.from(pkscript, "hex"),
            network: network
        });
        ret = address.address;
    } catch {
        /* try others */
    }
    try {
        let address = bitcoin.payments.p2wpkh({
            output: Buffer.from(pkscript, "hex"),
            network: network
        });
        ret = address.address;
    } catch {
        /* try others */
    }
    try {
        let address = bitcoin.payments.p2sh({
            output: Buffer.from(pkscript, "hex"),
            network: network
        });
        ret = address.address;
    } catch {
        /* try others */
    }
    try {
        let address = bitcoin.payments.p2pkh({
            output: Buffer.from(pkscript, "hex"),
            network: network
        });
        ret = address.address;
    } catch {
        /* end */
    }
    pkscript_to_address[pkscript] = ret;
    return ret;
}

const address_to_pkscript = {};
function pkscript_from_address(address) {
    if (address_to_pkscript[address]) {
        return address_to_pkscript[address];
    }
    let ret = null;
    try {
        let pkscript = bitcoin.address.toOutputScript(
            address,
            network
        );
        ret = pkscript.toString('hex');
    } catch {
        /* try others */
    }
    address_to_pkscript[address] = ret;

    return ret;
}

module.exports = {
    address_from_pkscript,
    pkscript_from_address,
};