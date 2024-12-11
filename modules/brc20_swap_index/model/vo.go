package model

import brc20Model "github.com/unisat-wallet/libbrc20-indexer/model"

// NFTData represents the structure for NFT data
type NFTData struct {
	InscriptionNumber int64  `json:"inscriptionNumber"` // Current inscription number
	InscriptionId     string `json:"inscriptionId"`     // Current inscription ID
	Offset            uint64 `json:"offset"`            // Satoshi offset in utxo

	HasMoved bool   `json:"moved"`    // Indicates if the NFT has been moved after creation
	Sequence uint16 `json:"sequence"` // If sequence > 0, the NFT has been moved after creation
	IsBRC20  bool   `json:"isBRC20"`  // Indicates if the NFT is BRC20
}

// TxStandardOutResp represents the response for a standard transaction output
type TxStandardOutResp struct {
	TxIdHex       string `json:"txid"`       // Current txid
	Vout          int    `json:"vout"`       // Current output sequence number
	Satoshi       int    `json:"satoshi"`    // Satoshi of the current output
	ScriptTypeHex string `json:"scriptType"` // Script type of the current output
	ScriptPkHex   string `json:"scriptPk"`   // Lock script of the current output
	CodeType      int    `json:"codeType"`   // Script type of the current output: 0: None, 1: FT, 2: Unique, 3: NFT, 4: CodeType_P2PK, 5: CodeType_P2PKH, 6: CodeType_P2SH, 7: CodeType_P2WPKH, 8: CodeType_P2WSH, 9: CodeType_P2TR
	Address       string `json:"address"`    // Address of the current output
	Height        int    `json:"height"`     // Block height where the transaction is packed
	TxIdx         int    `json:"idx"`        // Sequence number in the block of the spent txid
	OpInRBF       bool   `json:"isOpInRBF"`  // Indicates if the current transaction is an RBF (Replace-By-Fee) transaction
	IsSpent       bool   `json:"isSpent"`    // Indicates if the current transaction has been spent in the mempool

	CreatePointOfNFTs []*NFTCreatePoint `json:"-"`
	Inscriptions      []*NFTData        `json:"inscriptions"` // Positions of all inscriptions on the utxo
}

// InscriptionResp represents the response for an inscription
type InscriptionResp struct {
	UtxoOutpoint string             `json:"-"`       //
	UTXO         *TxStandardOutResp `json:"utxo"`    // UTXO results
	Address      string             `json:"address"` // Address of the current output
	CreateIdxKey string             `json:"-"`       //

	Offset            uint64 `json:"offset"`            // Satoshi offset in utxo
	InscriptionIndex  uint64 `json:"inscriptionIndex"`  // Current inscription index, indicating the sequence of repetition, 0 for first occurrence
	InscriptionNumber int64  `json:"inscriptionNumber"` // Current inscription number
	InscriptionId     string `json:"inscriptionId"`     // Current inscription ID

	ContentType   string `json:"contentType"`   //
	ContentLength int    `json:"contentLength"` //
	ContentBody   string `json:"contentBody"`   //
	Height        uint32 `json:"height"`        // Height at which the inscription was packed
	BlockTime     int    `json:"timestamp"`     // Block timestamp
	InSatoshi     int    `json:"inSatoshi"`     // Total input amount in the GenesisTx
	OutSatoshi    int    `json:"outSatoshi"`    // Total output amount in the GenesisTx

	BRC20 *brc20Model.InscriptionBRC20InfoResp `json:"brc20"` // BRC20 information, included only for valid transfers

	Detail *InscriptionContentForCheckResp `json:"detail"`
}
