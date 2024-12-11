package loader

import (
	"os"
	"sync"
	"testing"

	"github.com/unisat-wallet/libbrc20-indexer/model"
)

var (
	test_psqlInfo string
)

func init() {
	test_psqlInfo = "host=localhost port=5432 user=postgres password=postgres dbname=swap sslmode=disable"
	if info := os.Getenv("PG_CONN_INFO"); info != "" {
		test_psqlInfo = info
	}
	Init(test_psqlInfo)
}

func TestLoadBRC20InputDataFromDB(t *testing.T) {
	brc20Datas := make(chan *model.InscriptionBRC20Data, 1)

	wg := &sync.WaitGroup{}
	wg.Add(1)
	go func() {
		defer wg.Done()
		for data := range brc20Datas {
			if data.InscriptionNumber%1000 == 0 && !data.IsTransfer {
				if len(data.ContentBody) > 16 {
					data.ContentBody = data.ContentBody[:16]
				}
				t.Logf("number %d, height %d, brc20 data: %+v", data.InscriptionNumber, data.Height, data)
			}
		}
	}()

	if err := LoadBRC20InputDataFromDB(brc20Datas, 767400, 776000); err != nil {
		t.Fatal(err)
	}
	close(brc20Datas)
	wg.Wait()
}
