package utils

import (
	"encoding/hex"
	"errors"
	"os"
	"strings"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/btcsuite/btcd/btcutil"
	"github.com/btcsuite/btcd/chaincfg"
	"github.com/btcsuite/btcd/txscript"
	"golang.org/x/crypto/ripemd160"
)

func GetReversedStringHex(data string) (result string) {
	return hex.EncodeToString(ReverseBytes([]byte(data)))
}

func ReverseBytes(data []byte) (result []byte) {
	n := len(data)
	result = make([]byte, n)
	for i := 0; i < n; i++ {
		result[i] = data[n-1-i]
	}
	return result
}

const (
	PubKeyHashAddrIDMainNet = byte(0x00) // starts with 1
	PubKeyHashAddrIDTestNet = byte(0x6f) // starts with m or n

	P2SHAddrIDMainNet = byte(0x05) // starts with 3
	P2SHAddrIDTestNet = byte(0xc4) // starts with x

	PubKeyHashAddrHrpMainNet = "bc" // starts with bc1
	PubKeyHashAddrHrpTestNet = "tb" // starts with tb1
)

var (
	is_testnet          = os.Getenv("TESTNET")
	ErrChecksumMismatch = errors.New("checksum mismatch")
	empty               = make([]byte, ripemd160.Size)
	empty32             = make([]byte, 32)

	PubKeyHashAddrID  = PubKeyHashAddrIDMainNet
	P2SHAddrID        = P2SHAddrIDMainNet
	PubKeyHashAddrHrp = PubKeyHashAddrHrpMainNet
)

func init() {
	if is_testnet != "" {
		PubKeyHashAddrID = PubKeyHashAddrIDTestNet
		P2SHAddrID = P2SHAddrIDTestNet
		PubKeyHashAddrHrp = PubKeyHashAddrHrpTestNet
	}
}

func GetPkScriptByAddress(addr string) (pk []byte, err error) {
	if len(addr) == 0 {
		return nil, errors.New("decoded address empty")
	}

	netParams := &chaincfg.MainNetParams
	if is_testnet != "" {
		netParams = &chaincfg.TestNet3Params
	}
	addressObj, err := btcutil.DecodeAddress(addr, netParams)
	if err != nil {
		if len(addr) != 68 || !strings.HasPrefix(addr, "6a20") {
			return nil, errors.New("decoded address is of unknown format")
		}
		// check full hex
		pkHex, err := hex.DecodeString(addr)
		if err != nil {
			return nil, errors.New("decoded address is of unknown format")
		}
		return pkHex, nil
	}
	addressPkScript, err := txscript.PayToAddrScript(addressObj)
	if err != nil {
		return nil, errors.New("decoded address is of unknown format")
	}
	return addressPkScript, nil
}

// PayToTaprootScript creates a pk script for a pay-to-taproot output key.
func PayToTaprootScript(taprootKey *btcec.PublicKey) ([]byte, error) {
	return txscript.NewScriptBuilder().
		AddOp(txscript.OP_1).
		AddData(schnorr.SerializePubKey(taprootKey)).
		Script()
}

// PayToWitnessScript creates a pk script for a pay-to-wpkh output key.
func PayToWitnessScript(pubkey *btcec.PublicKey) ([]byte, error) {
	return txscript.NewScriptBuilder().
		AddOp(txscript.OP_0).
		AddData(btcutil.Hash160(pubkey.SerializeCompressed())).
		Script()
}
