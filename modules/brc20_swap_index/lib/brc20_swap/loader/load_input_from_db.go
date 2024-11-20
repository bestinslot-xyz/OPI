package loader

import (
	"brc20query/logger"
	"bytes"
	"context"
	"encoding/hex"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/unisat-wallet/libbrc20-indexer/model"
	"github.com/unisat-wallet/libbrc20-indexer/utils"
	"go.uber.org/zap"
)

func LoadBRC20InputDataFromDB(ctx context.Context, brc20Datas chan *model.InscriptionBRC20Data, startHeight int, endHeight int) error {
	logger.Log.Info("LoadBRC20InputDataFromDB", zap.Int("startHeight", startHeight), zap.Int("endHeight", endHeight))

	row := MetaDB.QueryRow(`select block_height from block_hashes order by block_height desc limit 1;`)
	var metaMaxHeight int
	if err := row.Scan(&metaMaxHeight); err != nil {
		return err
	}
	if endHeight <= 0 || metaMaxHeight < endHeight {
		endHeight = metaMaxHeight
	}

	for height := startHeight; height < endHeight; height++ {
		batchLimit := 10240
		st := time.Now()
		offset := 0
		count := 0
		blkDatas := make([]*model.InscriptionBRC20Data, 0)
		for ; ; offset += batchLimit {
			if datas, err := loadBRC20InputDataFromDBOnBatch(height, batchLimit, offset); err != nil {
				return err
			} else if len(datas) == 0 {
				break
			} else {
				blkDatas = append(blkDatas, datas...)
				count += len(datas)
				if len(datas) < batchLimit {
					break
				}
			}
		}

		if height%100 == 0 {
			logger.Log.Debug("LoadBRC20InputDataFromDB",
				zap.Int("height", height),
				zap.Int("count", count),
				zap.String("duration", time.Since(st).String()))
		}
		for _, data := range blkDatas {
			brc20Datas <- data
		}

		select {
		case <-ctx.Done():
			return nil
		default:
		}
	}
	return nil
}

// filter brc20
func isJson(contentBody []byte) bool {
	if len(contentBody) < 40 {
		return false
	}

	content := bytes.TrimSpace(contentBody)
	if !bytes.HasPrefix(content, []byte("{")) {
		return false
	}
	if !bytes.HasSuffix(content, []byte("}")) {
		return false
	}

	return true
}

func loadBRC20InputDataFromDBOnBatch(height int, queryLimit int, queryOffset int) (datas []*model.InscriptionBRC20Data, err error) {
	sql := fmt.Sprintf(`
SELECT ts.block_height, ts.inscription_id, ts.txcnt-1, ts.old_satpoint, ts.new_satpoint, ts.sent_as_fee, ts.sent_as_fee_txid, ts.new_output_value,
	ts.new_pkscript, n2id.inscription_number, c.content, c.content_type, h.block_time
FROM ord_transfers AS ts
INNER JOIN ord_number_to_id AS n2id ON ts.inscription_id = n2id.inscription_id
INNER JOIN ord_content AS c ON ts.inscription_id = c.inscription_id
INNER JOIN block_hashes AS h ON ts.block_height = h.block_height
WHERE ts.block_height = %d AND n2id.cursed_for_brc20 = false
ORDER BY ts.id LIMIT %d OFFSET %d
`, height, queryLimit, queryOffset)

	rows, err := MetaDB.Query(sql)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	for rows.Next() {
		var (
			block_height       uint32
			inscription_id     string
			txcnt              uint32
			old_satpoint       string
			new_satpoint       string
			sent_as_fee        bool
			sent_as_fee_txid   *string
			new_output_value   int64
			new_pkscript       string
			inscription_number int64
			content            []byte
			content_type       string
			block_time         uint32
		)

		if err := rows.Scan(
			&block_height, &inscription_id, &txcnt, &old_satpoint, &new_satpoint, &sent_as_fee, &sent_as_fee_txid, &new_output_value,
			&new_pkscript, &inscription_number, &content, &content_type, &block_time); err != nil {
			return datas, err
		}

		var (
			is_transfer bool   = false
			txid        string = ""
			vout        uint64 = 0
			offset      uint64 = 0
			contentBody []byte = nil
			contentType []byte = nil
			err         error  = nil
		)

		is_transfer = false
		if txcnt > 0 {
			is_transfer = true
		}

		contentBody = content
		contentType, err = hex.DecodeString(content_type)
		if err != nil {
			continue
		}
		if !isTextContentType(contentType) {
			continue
		}
		if !isJson(contentBody) {
			continue
		}

		if sent_as_fee {
			txid = *sent_as_fee_txid
			offset = 0
			new_output_value = 0
		} else {
			parts := strings.Split(new_satpoint, ":")
			txid = parts[0]

			vout, err = strconv.ParseUint(parts[1], 10, 64)
			if err != nil {
				logger.Log.Error("parse net_satpoint parts[1]",
					zap.String("error", err.Error()),
					zap.String("inscription_id", inscription_id),
					zap.String("new_satpoint", new_satpoint))
				return datas, err
			}

			offset, err = strconv.ParseUint(parts[2], 10, 64)
			if err != nil {
				logger.Log.Error("parse net_satpoint parts[2]",
					zap.String("error", err.Error()),
					zap.String("inscription_id", inscription_id),
					zap.String("new_satpoint", new_satpoint))
				return datas, err
			}
		}

		pkscript, err := hex.DecodeString(new_pkscript)
		if err != nil {
			logger.Log.Error(err.Error(),
				zap.String("inscription_id", inscription_id),
				zap.String("new_pkscript", new_pkscript))
			return datas, err
		}
		txidBytes, err := hex.DecodeString(txid)
		if err != nil {
			logger.Log.Error(err.Error(),
				zap.String("inscription_id", inscription_id),
				zap.String("txid", txid))
			return datas, err
		}

		idParts := strings.Split(inscription_id, "i")
		idx, err := strconv.Atoi(idParts[1])
		if err != nil {
			logger.Log.Error(err.Error(),
				zap.String("inscription_id", inscription_id),
			)
			return datas, err
		}

		data := model.InscriptionBRC20Data{
			IsTransfer:        is_transfer,
			TxId:              string(utils.ReverseBytes(txidBytes)),
			Idx:               uint32(idx),
			Vout:              uint32(vout),
			Offset:            offset,
			Satoshi:           uint64(new_output_value),
			PkScript:          string(pkscript),
			Fee:               0,
			InscriptionNumber: inscription_number,
			ContentBody:       contentBody,
			CreateIdxKey:      inscription_id,
			Height:            block_height,
			TxIdx:             0,
			BlockTime:         block_time,
			Sequence:          uint16(txcnt),
			InscriptionId:     inscription_id,
		}

		datas = append(datas, &data)
	}

	return datas, nil
}
