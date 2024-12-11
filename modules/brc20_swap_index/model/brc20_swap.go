package model

import swapModel "github.com/unisat-wallet/libbrc20-indexer/model"

type BRC20ModuleHistoryInfo struct {
	Type  string `json:"type"` // inscribe-deploy/inscribe-mint/inscribe-transfer/transfer/send/receive
	Valid bool   `json:"valid"`

	TxIdHex           string `json:"txid"`
	Idx               uint32 `json:"idx"` // inscription index
	Vout              uint32 `json:"vout"`
	Offset            uint64 `json:"offset"`
	InscriptionNumber int64  `json:"inscriptionNumber"`
	InscriptionId     string `json:"inscriptionId"`

	ContentType string `json:"contentType"`
	ContentBody string `json:"contentBody"`

	AddressFrom string `json:"from"`
	AddressTo   string `json:"to"`
	Satoshi     uint64 `json:"satoshi"`

	Data any `json:"data"`

	Height       uint32 `json:"height"`
	TxIdx        uint32 `json:"txidx"` // txidx in block
	BlockHashHex string `json:"blockhash"`
	BlockTime    uint32 `json:"blocktime"`
}

type BRC20ModuleHistoryResp struct {
	Height int                       `json:"height"` // synced block height
	Total  int                       `json:"total"`
	Cursor int                       `json:"cursor"`
	Detail []*BRC20ModuleHistoryInfo `json:"detail"`
}

type ModuleInscriptionInfoResp struct {
	UTXO    *TxStandardOutResp `json:"utxo"`    // utxo
	Address string             `json:"address"` // current output address

	Offset            uint64 `json:"offset"`            // sat offset in utxo
	InscriptionIndex  uint64 `json:"inscriptionIndex"`  // current inscriptionIndex，indicating the number of repetitions, 0 means first occurrence
	InscriptionNumber int64  `json:"inscriptionNumber"` // current inscriptionNumber
	InscriptionId     string `json:"inscriptionId"`     // current inscriptionId

	ContentType   string `json:"contentType"`   //
	ContentLength int    `json:"contentLength"` //
	ContentBody   string `json:"contentBody"`   //
	Height        uint32 `json:"height"`        // current height at which inscription was inscribed
	BlockTime     int    `json:"timestamp"`     //
	InSatoshi     int    `json:"inSatoshi"`     // total input amount in GenesisTx
	OutSatoshi    int    `json:"outSatoshi"`    // total output amount in GenesisTx

	Data map[string]string `json:"data"`
}

// commit verify
type BRC20ModuleVerifySwapCommitReq struct {
	CommitsStr  []string                                             `json:"commits"`
	CommitsObj  []*swapModel.InscriptionBRC20ModuleSwapCommitContent `json:"-"`
	LastResults []*swapModel.SwapFunctionResultCheckState            `json:"results"`
}

type BRC20ModuleVerifySwapCommitResp struct {
	Valid         bool   `json:"valid"`
	Critical      bool   `json:"critical"`
	FunctionIndex int    `json:"index"` // point out if invalid
	FunctionId    string `json:"id"`
	Message       string `json:"message"` //ok, or reason of invalid
}

// address info
type BRC20ModuleTickerStatusInfoOfAddressResp struct {
	Ticker string `json:"ticker"`

	ModuleAccountBalance string `json:"moduleAccountBalance"`
	SwapAccountBalance   string `json:"swapAccountBalance"`

	AvailableBalance       string `json:"availableBalance"`
	ApproveableBalance     string `json:"approveableBalance"`
	CondApproveableBalance string `json:"condApproveableBalance"`
	ReadyToWithdrawAmount  string `json:"readyToWithdrawAmount"`

	HistoryCount int `json:"historyCount"`
}
