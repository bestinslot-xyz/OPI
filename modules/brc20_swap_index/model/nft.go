package model

type InscriptionContentForCheckResp struct {
	InscriptionType    string `json:"inscriptionType"`
	InscriptionNumber  int64  `json:"inscriptionNumber"`
	InscriptionId      string `json:"inscriptionId"`
	InscriptionName    string `json:"inscriptionName"`
	InscriptionNameHex string `json:"inscriptionNameHex"`
	BlockTime          int    `json:"timestamp"`
}
