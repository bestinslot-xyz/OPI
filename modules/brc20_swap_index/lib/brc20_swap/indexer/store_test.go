package indexer

import (
	"os"
	"testing"

	"brc20query/lib/brc20_swap/decimal"
	"brc20query/lib/brc20_swap/model"
)

var (
	psqlInfo string
)

func init() {
	psqlInfo = "host=localhost port=5432 user=postgres password=postgres dbname=postgres sslmode=disable"
	if info := os.Getenv("PG_CONN_INFO"); info != "" {
		psqlInfo = info
	}
}

func TestSavaDataToDb(t *testing.T) {
	createKey1 := model.NFTCreateIdxKey{
		Height:     820000,
		IdxInBlock: 0,
	}
	createKey2 := model.NFTCreateIdxKey{
		Height:     830000,
		IdxInBlock: 0,
	}

	g := &BRC20ModuleIndexer{
		InscriptionsValidTransferMap: map[string]*model.InscriptionBRC20TickInfo{
			createKey1.String(): &model.InscriptionBRC20TickInfo{
				Tick: "ordi", TxId: "txid", Vout: 1, Satoshi: 1, Offset: 0, PkScript: "pkscript",
				InscriptionNumber: 1,
				Amount:            decimal.MustNewDecimalFromString("10000000000000000000", 18),
				Meta:              &model.InscriptionBRC20Data{InscriptionId: "ordi"},
			},
		},
		InscriptionsInvalidTransferMap: map[string]*model.InscriptionBRC20TickInfo{
			createKey2.String(): &model.InscriptionBRC20TickInfo{
				Tick: "ordi", TxId: "txid", Vout: 1, Satoshi: 1, Offset: 0, PkScript: "pkscript",
				InscriptionNumber: 1,
				Amount:            decimal.MustNewDecimalFromString("10000000000000000000", 18),
				Meta:              &model.InscriptionBRC20Data{InscriptionId: "ordi"},
			},
		},
		ModulesInfoMap: map[string]*model.BRC20ModuleSwapInfo{
			"module1": &model.BRC20ModuleSwapInfo{
				ID:   "0x123456789",
				Name: "brc20-swap",
			},
		},
	}
	g.SaveDataToDB(psqlInfo, 0)
}

func TestLoadDataFromDb(t *testing.T) {
	g := &BRC20ModuleIndexer{}
	g.LoadDataFromDB(psqlInfo, 0)
}
