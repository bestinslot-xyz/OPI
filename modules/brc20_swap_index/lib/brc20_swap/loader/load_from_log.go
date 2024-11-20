package loader

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"os"
	"strconv"
	"strings"

	"github.com/unisat-wallet/libbrc20-indexer/model"
	"github.com/unisat-wallet/libbrc20-indexer/utils"
)

func isTextContentType(contenttype []byte) bool {
	if bytes.HasPrefix(contenttype, []byte("text/plain;")) {
		return true
	}
	if bytes.Equal(contenttype, []byte("text/plain")) {
		return true
	}
	if bytes.Equal(contenttype, []byte("application/json")) {
		return true
	}
	return false
}

// LoadBRC20InputDataFromOrdLog log_file.txt
func LoadBRC20InputDataFromOrdLog(fname string, brc20Datas chan *model.InscriptionBRC20Data, startHeight, endHeight int) error {
	file, err := os.Open(fname)
	if err != nil {
		return err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	max := 128 * 1024 * 1024
	buf := make([]byte, max)
	scanner.Buffer(buf, max)

	number2IdLen := len("cmd;825399;insert;number_to_id;")
	contentLen := len("cmd;825399;insert;content;")
	transferLen := len("cmd;825399;insert;transfer;")
	sentAsFeeLen := len("cmd;825399;insert;early_transfer_sent_as_fee;")

	id2number := make(map[string]int, 0)
	id2content := make(map[string]string, 0)
	id2parent := make(map[string]string, 0)
	blocktime := 0

	for scanner.Scan() {
		line := scanner.Text()

		// reset number/content
		if strings.Contains(line, ";block_start;") {
			id2number = make(map[string]int, 0)
			id2content = make(map[string]string, 0)
			id2parent = make(map[string]string, 0)

			fields := strings.Split(line, ";")
			if len(fields) != 4 {
				continue
			}
			blocktimeStr := fields[3]
			blocktime, err = strconv.Atoi(blocktimeStr)
			if err != nil {
				continue
			}
			continue
		}

		lineLen := len(line)

		// number to id
		if lineLen > number2IdLen && strings.Contains(line[:number2IdLen], "number_to_id") {
			fields := strings.Split(line, ";")
			if len(fields) != 8 {
				continue
			}
			// cmd, HEIGHT, insert, number_to_id, NUM, ID, CURSED, parent = fields
			if fields[6] == "1" { // cursed
				continue
			}

			number, err := strconv.Atoi(fields[4]) // number
			if err != nil {
				continue
			}

			idStr := fields[5]
			id2number[idStr] = number
			id2parent[idStr] = fields[7]
			continue
		}

		// content
		if lineLen > contentLen && strings.Contains(line[:contentLen], "content") {
			fields := strings.SplitN(line, ";", 9)
			if len(fields) != 9 {
				continue
			}
			// cmd, HEIGHT, insert, content, ID, isjson, contentTypeHex, metadata, contentBody = fields
			contentBody := fields[8]
			if len(contentBody) < 10 {
				continue
			}
			if len(contentBody) > 400*1024*1024 {
				continue
			}

			if fields[5] != "true" { // isjson
				continue
			}

			contentType, err := hex.DecodeString(fields[6])
			if err != nil {
				continue
			}
			if !isTextContentType(contentType) {
				continue
			}
			idStr := fields[4]
			id2content[idStr] = contentBody

			continue
		}

		// sent as fee
		if lineLen > sentAsFeeLen && strings.Contains(line[:sentAsFeeLen], "early_transfer_sent_as_fee") {
			fields := strings.Split(line, ";")
			if len(fields) != 6 {
				continue
			}
			// cmd, HEIGHT, insert, sentasfee, ID, txid = fields

			heightStr := fields[1]
			height, err := strconv.Atoi(heightStr)
			if err != nil {
				continue
			}
			if int(height) >= endHeight {
				break
			}
			if int(height) < startHeight {
				continue
			}

			idStr := fields[4]
			idParts := strings.Split(idStr, "i")
			idx, err := strconv.Atoi(idParts[1])
			if err != nil {
				continue
			}

			txid, err := hex.DecodeString(fields[5])
			if err != nil {
				return err
			}

			data := &model.InscriptionBRC20Data{
				IsTransfer: true,
				TxId:       string(utils.ReverseBytes(txid)),
				Idx:        uint32(idx),
				Vout:       0,
				Offset:     0,

				Satoshi:  0,
				PkScript: "",
				Fee:      0,

				InscriptionNumber: 0,
				ContentBody:       nil,
				CreateIdxKey:      idStr,
				Parent:            nil,

				Height:    uint32(height),
				BlockTime: uint32(blocktime),
				TxIdx:     1, // fixme
				Sequence:  1, // fixme
			}

			brc20Datas <- data
			continue
		}

		// transfer
		if lineLen < transferLen || !strings.Contains(line[:transferLen], "transfer") {
			continue
		}

		fields := strings.Split(line, ";")
		if len(fields) != 11 {
			continue
		}
		// cmd, HEIGHT, insert, transfer, ID, OLDPOINT, NEWPOINT, ISTOFEE, pkScriptHex, satoshi, txcnt = fields
		heightStr := fields[1]
		height, err := strconv.Atoi(heightStr)
		if err != nil {
			continue
		}
		if int(height) >= endHeight {
			break
		}
		if int(height) < startHeight {
			continue
		}

		sequenceStr := fields[10]
		sequence, err := strconv.Atoi(sequenceStr)
		if err != nil {
			continue
		}
		sequence -= 1

		satoshiStr := fields[9]
		satoshi, err := strconv.Atoi(satoshiStr)
		if err != nil {
			continue
		}

		pkScript, err := hex.DecodeString(fields[8])
		if err != nil {
			return err
		}

		if fields[7] == "true" { // isToFee
			continue
		}

		newPointParts := strings.Split(fields[6], ":")
		txid, err := hex.DecodeString(newPointParts[0])
		if err != nil {
			return err
		}
		vout, err := strconv.Atoi(newPointParts[1])
		if err != nil {
			continue
		}
		offset, err := strconv.Atoi(newPointParts[2])
		if err != nil {
			continue
		}

		idStr := fields[4]
		idParts := strings.Split(idStr, "i")
		idx, err := strconv.Atoi(idParts[1])
		if err != nil {
			continue
		}

		number := 0
		if _, ok := id2number[idStr]; ok {
			number = id2number[idStr]
		}
		contentBody := ""
		if _, ok := id2content[idStr]; ok {
			contentBody = id2content[idStr]
		}

		parent := ""
		if sequence == 0 {
			if _, ok := id2parent[idStr]; ok {
				parent = id2parent[idStr]
			}
		}
		if sequence == 0 && (contentBody == "" || number == 0) {
			continue
		}

		data := &model.InscriptionBRC20Data{
			IsTransfer: (sequence > 0),
			TxId:       string(utils.ReverseBytes(txid)),
			Idx:        uint32(idx),
			Vout:       uint32(vout),
			Offset:     uint64(offset),

			Satoshi:  uint64(satoshi),
			PkScript: string(pkScript),
			Fee:      0,

			InscriptionNumber: int64(number),
			ContentBody:       []byte(contentBody),
			CreateIdxKey:      idStr,
			Parent:            []byte(GetNFTIdForScript(parent)),

			Height:    uint32(height),
			BlockTime: uint32(blocktime),
			TxIdx:     1, // fixme
			Sequence:  uint16(sequence),
		}

		brc20Datas <- data
	}

	if err := scanner.Err(); err != nil {
		return err
	}

	return nil
}

func GetNFTIdForScript(inscriptionId string) (scriptId string) {
	idParts := strings.Split(inscriptionId, "i")
	if len(idParts) != 2 {
		return ""
	}
	txidBin, err := hex.DecodeString(idParts[0])
	if err != nil {
		return ""
	}
	txid := string(utils.ReverseBytes(txidBin))
	idx, err := strconv.Atoi(idParts[1])
	if err != nil {
		return ""
	}

	if idx == 0 {
		return txid
	}

	var n [4]byte
	binary.LittleEndian.PutUint32(n[:], uint32(idx))

	if idx < 256 {
		return txid + string(n[:1])
	} else if idx < 65536 {
		return txid + string(n[:2])
	} else if idx < 16777216 {
		return txid + string(n[:3])
	} else {
		return txid + string(n[:])
	}
}
