package brc20_swap

import (
	"testing"

	"github.com/btcsuite/btcd/chaincfg"
	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/event"
	"github.com/unisat-wallet/libbrc20-indexer/indexer"
	"github.com/unisat-wallet/libbrc20-indexer/loader"
)

var LOAD_TESTNET = false
var LOAD_EVENTS = true

// go test -v -run TestBRC20Swap\$ .
func TestBRC20Swap(t *testing.T) {

	if LOAD_TESTNET {
		conf.GlobalNetParams = &chaincfg.TestNet3Params
		conf.TICKS_ENABLED = "sats ordi trac oshi btcs oxbt texo cncl meme honk zbit vmpx pepe mxrc   doge eyee"

		conf.TICKS_ENABLED = "sats ordi trac oshi btcs oxbt texo cncl meme honk zbit vmpx pepe mxrc   doge eyee test 💰 your domo"

		conf.TICKS_ENABLED = ""
		conf.MODULE_SWAP_SOURCE_INSCRIPTION_ID = "eabfbf7cba3509134582c2216709527ddde716d3be96beababc16c8f28d5fd31i0"
	} else {
		// mainnet
		conf.GlobalNetParams = &chaincfg.MainNetParams
		conf.TICKS_ENABLED = "sats ordi trac oshi btcs oxbt texo cncl meme honk zbit vmpx pepe mxrc   doge eyee"
		conf.MODULE_SWAP_SOURCE_INSCRIPTION_ID = "93ce120ff87364c261a534fea4c39196a615f449412fb3547a185d92306a39b8i0"
	}

	// brc20Datas, err := loader.LoadBRC20InputJsonData("./data/brc20swap.input.conf")
	// if err != nil {
	// 	t.Logf("load json failed: %s", err)
	// }
	// loader.DumpBRC20InputData("./data/brc20swap.input.txt", brc20Datas, false)

	// if err := indexer.InitResultDataFromFile("./data/brc20swap.results.json"); err != nil {
	// 	t.Logf("load json failed: %s", err)
	// }

	brc20Datas := make(chan interface{}, 0)

	var err error
	if LOAD_EVENTS {
		conf.DEBUG = true

		t.Logf("start loading event")
		if datas, err := event.InitTickDataFromFile("./data/brc20swap.ticks.json"); err != nil {
			t.Logf("load tick json failed: %s", err)
			return
		} else {
			for _, d := range datas {
				brc20Datas <- d
			}
			close(brc20Datas)
		}
		if datas, err := event.GenerateBRC20InputDataFromEvents("./data/brc20swap.events.json"); err != nil {
			t.Logf("load event json failed: %s", err)
			return
		} else {
			for _, d := range datas {
				brc20Datas <- d
			}
			close(brc20Datas)
		}
		loader.DumpBRC20InputData("./data/brc20swap.events.input.txt", brc20Datas, false)

	} else {
		t.Logf("start loading data")

		if err = loader.LoadBRC20InputData("./data/brc20swap.input.txt", brc20Datas); err != nil {
			t.Logf("load json failed: %s", err)
		}

	}
	t.Logf("start init")

	g := &indexer.BRC20ModuleIndexer{}
	g.Init()
	g.ProcessUpdateLatestBRC20Loop(brc20Datas, nil)

	// next half
	t.Logf("start deep copy")
	// newg := g.DeepCopy()
	newg := g

	t.Logf("start process")
	newg.ProcessUpdateLatestBRC20Loop(nil, nil)

	t.Logf("dump swap")
	loader.DumpTickerInfoMap("./data/brc20swap.output.txt",
		newg.HistoryData,
		newg.InscriptionsTickerInfoMap,
		newg.UserTokensBalanceData,
		newg.TokenUsersBalanceData,
	)

	t.Logf("dump module")
	loader.DumpModuleInfoMap("./data/brc20module.output.txt",
		newg.ModulesInfoMap,
	)

}
