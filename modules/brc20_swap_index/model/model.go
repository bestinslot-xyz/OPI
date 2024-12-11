package model

import (
	"encoding/binary"
	"encoding/json"
)

const MEMPOOL_HEIGHT = 0x3fffff // 4294967295 2^32-1; 3fffff, 2^22-1
const HEIGHT_MUTIPLY = 1000000000

type Response struct {
	Code int         `json:"code"`
	Msg  string      `json:"msg"`
	Data interface{} `json:"data"`
}

func (t *Response) MarshalJSON() ([]byte, error) {
	return json.Marshal(*t)
}

// nft create point on create
type NFTCreatePoint struct {
	Height     uint32 // Height of NFT show in block onCreate
	IdxInBlock uint64 // Index of NFT show in block onCreate
	Offset     uint64 // sat offset in utxo
	Sequence   uint16 // sequence>0 the NFT has been moved after created
	Vindicate  bool   // the NFT is Vindicate
	IsBRC20    bool   // the NFT is BRC20
	IsText     bool   // the NFT is Text

	ContentType []byte
	Content     []byte
}

func (p *NFTCreatePoint) GetCreateIdxKey() string {
	var key [12]byte
	binary.LittleEndian.PutUint32(key[0:4], p.Height)
	binary.LittleEndian.PutUint64(key[4:12], p.IdxInBlock)
	return string(key[:])
}
