package brc20

import (
	"brc20query/lib/utils"
	"brc20query/logger"
	"brc20query/model"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"strings"

	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/constant"
	brc20Model "github.com/unisat-wallet/libbrc20-indexer/model"
	swapModel "github.com/unisat-wallet/libbrc20-indexer/model"
	brc20Utils "github.com/unisat-wallet/libbrc20-indexer/utils"
	"go.uber.org/zap"
)

// for api, holders
func GetBRC20TickerHolders(ticker string, start, size int) (total int, nftsRsp []*model.BRC20TickerHoldersInfo, err error) {
	logger.Log.Info("GetBRC20TickerHolders",
		zap.String("ticker", ticker),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	tokenUsers, ok := model.GSwap.TokenUsersBalanceData[ticker]
	if !ok {
		return 0, make([]*model.BRC20TickerHoldersInfo, 0), nil
	}

	total = len(tokenUsers)
	if start >= total {
		return total, make([]*model.BRC20TickerHoldersInfo, 0), nil
	}

	var holdersBalance []*brc20Model.BRC20TokenBalance
	for _, balance := range tokenUsers {
		holdersBalance = append(holdersBalance, balance)
	}

	sort.Slice(holdersBalance, func(i, j int) bool {
		return strings.Compare(holdersBalance[i].PkScript, holdersBalance[j].PkScript) > 0
	})

	sort.SliceStable(holdersBalance, func(i, j int) bool {
		return holdersBalance[i].OverallBalance().Cmp(holdersBalance[j].OverallBalance()) > 0
	})

	for idx, balance := range holdersBalance[start:] {
		if idx >= size {
			break
		}

		address, err := brc20Utils.GetAddressFromScript([]byte(balance.PkScript), conf.GlobalNetParams)
		if err != nil {
			address = hex.EncodeToString([]byte(balance.PkScript))
		}

		nftsRsp = append(nftsRsp, &model.BRC20TickerHoldersInfo{
			Address:                address,
			OverallBalance:         balance.OverallBalance().String(),
			TransferableBalance:    balance.TransferableBalance.String(),
			AvailableBalance:       balance.AvailableBalance.String(),
			AvailableBalanceSafe:   balance.AvailableBalanceSafe.String(),
			AvailableBalanceUnSafe: balance.AvailableBalance.Sub(balance.AvailableBalanceSafe).String(),
		})
	}

	return total, nftsRsp, nil
}

func GetBRC20Status(ticker, completeType, sortBy string, start, size int) (total int, nftsRsp []*model.BRC20TickerStatusInfo, err error) {
	logger.Log.Info("GetBRC20Status",
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	var statusInfo []*brc20Model.BRC20TokenInfo
	for _, info := range model.GSwap.InscriptionsTickerInfoMap {
		if ticker != "" {
			uniqueLowerTicker := strings.ToLower(info.Deploy.Data.BRC20Tick)
			if !strings.Contains(uniqueLowerTicker, ticker) {
				continue
			}
		}

		if completeType == "yes" && info.Deploy.TotalMinted.Cmp(info.Deploy.Max) == 0 {
			statusInfo = append(statusInfo, info)
		}
		if completeType == "no" && info.Deploy.TotalMinted.Cmp(info.Deploy.Max) < 0 {
			statusInfo = append(statusInfo, info)
		}
		if completeType == "" {
			statusInfo = append(statusInfo, info)
		}
	}

	total = len(statusInfo)
	if start >= total {
		return total, make([]*model.BRC20TickerStatusInfo, 0), nil
	}

	// sort by deploy height
	sort.Slice(statusInfo, func(i, j int) bool {
		idxi := uint64(statusInfo[i].Deploy.Height)*4294967296 + uint64(statusInfo[i].Deploy.TxIdx)
		idxj := uint64(statusInfo[j].Deploy.Height)*4294967296 + uint64(statusInfo[j].Deploy.TxIdx)
		return idxi < idxj
	})

	if sortBy == "holders" {
		sort.SliceStable(statusInfo, func(i, j int) bool {
			holdersi := len(model.GSwap.TokenUsersBalanceData[strings.ToLower(statusInfo[i].Ticker)])
			holdersj := len(model.GSwap.TokenUsersBalanceData[strings.ToLower(statusInfo[j].Ticker)])
			return holdersi > holdersj
		})
	} else if sortBy == "transactions" {
		sort.SliceStable(statusInfo, func(i, j int) bool {
			return len(statusInfo[i].History) > len(statusInfo[j].History)
		})

	} else if sortBy == "minted" {
		// sort by progress
		sort.SliceStable(statusInfo, func(i, j int) bool {
			ratei := float64(0)
			ratej := float64(0)
			maxi := statusInfo[i].Deploy.Max.Float64()
			maxj := statusInfo[j].Deploy.Max.Float64()
			if maxi != 0 {
				ratei = statusInfo[i].Deploy.TotalMinted.Float64() / maxi
			}
			if maxj != 0 {
				ratej = statusInfo[j].Deploy.TotalMinted.Float64() / maxj
			}
			return ratei > ratej
		})
	} else if sortBy == "deploy" {
		// sort by deploy height
		// sort.Slice(statusInfo, func(i, j int) bool {
		// 	idxi := uint64(statusInfo[i].Deploy.Height)*4294967296 + uint64(statusInfo[i].Deploy.TxIdx)
		// 	idxj := uint64(statusInfo[j].Deploy.Height)*4294967296 + uint64(statusInfo[j].Deploy.TxIdx)
		// 	return idxi < idxj
		// })
	}
	// sort by mintedtimes
	// sort.Slice(statusInfo, func(i, j int) bool {
	// 	return statusInfo[i].Deploy.MintTimes > statusInfo[j].Deploy.MintTimes
	// })

	for idx, info := range statusInfo[start:] {
		if size > 0 && idx >= size {
			break
		}

		uniqueLowerTicker := strings.ToLower(info.Deploy.Data.BRC20Tick)

		nftsRsp = append(nftsRsp, &model.BRC20TickerStatusInfo{
			Ticker:       info.Deploy.Data.BRC20Tick,
			SelfMint:     info.Deploy.SelfMint,
			HoldersCount: len(model.GSwap.TokenUsersBalanceData[uniqueLowerTicker]),
			HistoryCount: len(info.History),

			InscriptionNumber: info.Deploy.InscriptionNumber,
			InscriptionId:     info.Deploy.GetInscriptionId(),

			Max:   info.Deploy.Max.String(),
			Limit: info.Deploy.Limit.String(),

			Minted:             info.Deploy.TotalMinted.String(),
			TotalMinted:        info.Deploy.TotalMinted.String(),
			ConfirmedMinted:    info.Deploy.ConfirmedMinted.String(),
			ConfirmedMinted1h:  info.Deploy.ConfirmedMinted1h.String(),
			ConfirmedMinted24h: info.Deploy.ConfirmedMinted24h.String(),

			MintTimes: info.Deploy.MintTimes,
			Decimal:   info.Deploy.Decimal,

			DeployHeight:    info.Deploy.Height,
			DeployBlockTime: info.Deploy.BlockTime,

			CompleteHeight:    info.Deploy.CompleteHeight,
			CompleteBlockTime: info.Deploy.CompleteBlockTime,

			InscriptionNumberStart: info.Deploy.InscriptionNumberStart,
			InscriptionNumberEnd:   info.Deploy.InscriptionNumberEnd,
		})
	}

	return total, nftsRsp, nil
}

func GetBRC20List(start, size int) (total int, nftsRsp []string, err error) {
	logger.Log.Info("GetBRC20List",
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	var statusInfo []*brc20Model.BRC20TokenInfo
	for _, info := range model.GSwap.InscriptionsTickerInfoMap {
		statusInfo = append(statusInfo, info)
	}

	total = len(statusInfo)
	if start >= total {
		return total, make([]string, 0), nil
	}

	// sort by deploy height
	sort.Slice(statusInfo, func(i, j int) bool {
		idxi := uint64(statusInfo[i].Deploy.Height)*4294967296 + uint64(statusInfo[i].Deploy.TxIdx)
		idxj := uint64(statusInfo[j].Deploy.Height)*4294967296 + uint64(statusInfo[j].Deploy.TxIdx)
		return idxi < idxj
	})

	for idx, info := range statusInfo[start:] {
		if size > 0 && idx >= size {
			break
		}
		nftsRsp = append(nftsRsp, info.Deploy.Data.BRC20Tick)
	}

	return total, nftsRsp, nil
}

func GetBRC20TickerInfo(ticker string) (nftRsp *model.BRC20TickerStatusInfo, err error) {
	logger.Log.Info("GetBRC20TickerInfo", zap.String("ticker", ticker))

	if model.GSwap == nil {
		return nil, errors.New("brc20 not ready")
	}

	tokenInfo, ok := model.GSwap.InscriptionsTickerInfoMap[ticker]
	if !ok {
		return nil, errors.New("ticker invalid")
	}

	uniqueLowerTicker := strings.ToLower(tokenInfo.Deploy.Data.BRC20Tick)

	creatorAddress, err := brc20Utils.GetAddressFromScript([]byte(tokenInfo.Deploy.PkScript), conf.GlobalNetParams)
	if err != nil {
		creatorAddress = hex.EncodeToString([]byte(tokenInfo.Deploy.PkScript))
	}

	nftRsp = &model.BRC20TickerStatusInfo{
		Ticker:       tokenInfo.Deploy.Data.BRC20Tick,
		SelfMint:     tokenInfo.Deploy.SelfMint,
		HoldersCount: len(model.GSwap.TokenUsersBalanceData[uniqueLowerTicker]),
		HistoryCount: len(tokenInfo.History),

		InscriptionNumber: tokenInfo.Deploy.InscriptionNumber,
		InscriptionId:     tokenInfo.Deploy.GetInscriptionId(),

		Max:   tokenInfo.Deploy.Max.String(),
		Limit: tokenInfo.Deploy.Limit.String(),

		Minted:             tokenInfo.Deploy.TotalMinted.String(),
		TotalMinted:        tokenInfo.Deploy.TotalMinted.String(),
		ConfirmedMinted:    tokenInfo.Deploy.ConfirmedMinted.String(),
		ConfirmedMinted1h:  tokenInfo.Deploy.ConfirmedMinted1h.String(),
		ConfirmedMinted24h: tokenInfo.Deploy.ConfirmedMinted24h.String(),

		MintTimes: tokenInfo.Deploy.MintTimes,
		Decimal:   tokenInfo.Deploy.Decimal,

		CreatorAddress:  creatorAddress,
		TxIdHex:         utils.GetReversedStringHex(tokenInfo.Deploy.TxId),
		DeployHeight:    tokenInfo.Deploy.Height,
		DeployBlockTime: tokenInfo.Deploy.BlockTime,

		CompleteHeight:    tokenInfo.Deploy.CompleteHeight,
		CompleteBlockTime: tokenInfo.Deploy.CompleteBlockTime,

		InscriptionNumberStart: tokenInfo.Deploy.InscriptionNumberStart,
		InscriptionNumberEnd:   tokenInfo.Deploy.InscriptionNumberEnd,
	}

	return nftRsp, nil
}

// for api, all history
func GetBRC20AllHistoryByHeight(height, start, size int) (total int, nftsRsp []*model.BRC20TickerHistoryInfo, err error) {
	logger.Log.Info("GetBRC20AllHistoryByHeight",
		zap.Int("height", height),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	firstHistoryByHeight := model.GSwap.FirstHistoryByHeight
	var tokenInfoHistory []uint32 = model.GSwap.AllHistory

	firstHistory := firstHistoryByHeight[uint32(height)]
	lastHistory := firstHistoryByHeight[uint32(height+1)]

	// from current history
	var tokenInfoHistoryHeight []uint32
	for idx := len(tokenInfoHistory) - 1; idx >= 0; idx-- {
		history := tokenInfoHistory[idx]

		if history < firstHistory {
			break
		}
		if lastHistory > 0 && history >= lastHistory {
			continue
		}
		tokenInfoHistoryHeight = append(tokenInfoHistoryHeight, history)
	}

	// reverse
	for i, j := 0, len(tokenInfoHistoryHeight)-1; i < j; i, j = i+1, j-1 {
		tokenInfoHistoryHeight[i], tokenInfoHistoryHeight[j] = tokenInfoHistoryHeight[j], tokenInfoHistoryHeight[i]
	}
	tokenInfoHistory = tokenInfoHistoryHeight

	total = len(tokenInfoHistory)
	if start >= total {
		return total, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	end := start + size
	if end > total {
		end = total
	}
	for _, historyIdx := range tokenInfoHistory[start:end] {

		buf := model.GSwap.HistoryData[historyIdx]
		history := &swapModel.BRC20History{}
		history.Unmarshal(buf)

		if height != int(history.Height) {
			continue
		}

		addressFrom, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptFrom), conf.GlobalNetParams)
		if err != nil {
			addressFrom = hex.EncodeToString([]byte(history.PkScriptFrom))
		}

		addressTo, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptTo), conf.GlobalNetParams)
		if err != nil {
			addressTo = hex.EncodeToString([]byte(history.PkScriptTo))
		}

		blockhash := ""
		if int(history.Height) < len(model.GlobalBlocksHash) {
			blockhash = utils.GetReversedStringHex(model.GlobalBlocksHash[history.Height])
		}

		nftsRsp = append(nftsRsp, &model.BRC20TickerHistoryInfo{
			Ticker: history.Inscription.Data.BRC20Tick,
			Type:   constant.BRC20_HISTORY_TYPE_NAMES[history.Type],
			Valid:  history.Valid,

			InscriptionNumber: history.Inscription.InscriptionNumber,
			InscriptionId:     history.Inscription.InscriptionId,
			TxIdHex:           utils.GetReversedStringHex(history.TxId),
			Vout:              history.Vout,
			Offset:            history.Offset,
			Idx:               history.Idx,

			AddressFrom:         addressFrom,
			AddressTo:           addressTo,
			Satoshi:             history.Satoshi,
			Fee:                 history.Fee,
			Amount:              history.Amount,
			OverallBalance:      history.OverallBalance,
			TransferableBalance: history.TransferableBalance,
			AvailableBalance:    history.AvailableBalance,

			Height:       history.Height,
			TxIdx:        history.TxIdx,
			BlockHashHex: blockhash,
			BlockTime:    history.BlockTime,
		})
	}

	return total, nftsRsp, nil
}

// for api, all history
func GetBRC20AllHistoryByAddress(pkScript []byte, start, size int) (total int, nftsRsp []*model.BRC20TickerHistoryInfo, err error) {
	logger.Log.Info("GetBRC20AllHistoryByAddress",
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	var tokenInfoHistory []uint32 = model.GSwap.GetBRC20HistoryByUserForAPI(string(pkScript)).History

	total = len(tokenInfoHistory)
	if start >= total {
		return total, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	end := start + size
	if end > total {
		end = total
	}

	// from current history
	var tokenInfoHistoryByAddress []uint32

	count := 0
	if start < len(tokenInfoHistory) {
		for idx := len(tokenInfoHistory) - 1 - start; idx >= 0 && count < (end-start); idx-- {
			history := tokenInfoHistory[idx]

			tokenInfoHistoryByAddress = append(tokenInfoHistoryByAddress, history)
			count++
		}
		start = 0
		end -= len(tokenInfoHistory)
	} else {
		start -= len(tokenInfoHistory)
		end -= len(tokenInfoHistory)
	}

	for _, historyIdx := range tokenInfoHistoryByAddress {
		buf := model.GSwap.HistoryData[historyIdx]
		history := &swapModel.BRC20History{}
		history.Unmarshal(buf)

		addressFrom, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptFrom), conf.GlobalNetParams)
		if err != nil {
			addressFrom = hex.EncodeToString([]byte(history.PkScriptFrom))
		}

		addressTo, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptTo), conf.GlobalNetParams)
		if err != nil {
			addressTo = hex.EncodeToString([]byte(history.PkScriptTo))
		}

		blockhash := ""
		if int(history.Height) < len(model.GlobalBlocksHash) {
			blockhash = utils.GetReversedStringHex(model.GlobalBlocksHash[history.Height])
		}

		nftsRsp = append(nftsRsp, &model.BRC20TickerHistoryInfo{
			Ticker: history.Inscription.Data.BRC20Tick,
			Type:   constant.BRC20_HISTORY_TYPE_NAMES[history.Type],
			Valid:  history.Valid,

			InscriptionNumber: history.Inscription.InscriptionNumber,
			InscriptionId:     history.Inscription.InscriptionId,
			TxIdHex:           utils.GetReversedStringHex(history.TxId),
			Vout:              history.Vout,
			Offset:            history.Offset,
			Idx:               history.Idx,

			AddressFrom:         addressFrom,
			AddressTo:           addressTo,
			Satoshi:             history.Satoshi,
			Fee:                 history.Fee,
			Amount:              history.Amount,
			OverallBalance:      history.OverallBalance,
			TransferableBalance: history.TransferableBalance,
			AvailableBalance:    history.AvailableBalance,

			Height:       history.Height,
			TxIdx:        history.TxIdx,
			BlockHashHex: blockhash,
			BlockTime:    history.BlockTime,
		})
	}

	return total, nftsRsp, nil
}

// for api, history
func GetBRC20TickerHistory(historyType, ticker string, height, start, size int) (total int, nftsRsp []*model.BRC20TickerHistoryInfo, err error) {
	logger.Log.Info("GetBRC20TickerHistory",
		zap.String("type", historyType),
		zap.String("ticker", ticker),
		zap.Int("height", height),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	firstHistoryByHeight := model.GSwap.FirstHistoryByHeight
	tokenInfo, ok := model.GSwap.InscriptionsTickerInfoMap[ticker]
	if !ok {
		return 0, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	var tokenInfoHistory []uint32 = tokenInfo.History
	if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_MINT {
		tokenInfoHistory = tokenInfo.HistoryMint
	} else if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_TRANSFER {
		tokenInfoHistory = tokenInfo.HistoryInscribeTransfer
	} else if historyType == constant.BRC20_HISTORY_TYPE_TRANSFER {
		tokenInfoHistory = tokenInfo.HistoryTransfer
	}

	if height > 0 {
		firstHistory := firstHistoryByHeight[uint32(height)]
		lastHistory := firstHistoryByHeight[uint32(height+1)]

		var tokenInfoHistoryHeight []uint32
		for idx := len(tokenInfoHistory) - 1; idx >= 0; idx-- {
			history := tokenInfoHistory[idx]

			if history < firstHistory {
				break
			}
			if lastHistory > 0 && history >= lastHistory {
				continue
			}
			tokenInfoHistoryHeight = append(tokenInfoHistoryHeight, history)
		}
		for i, j := 0, len(tokenInfoHistoryHeight)-1; i < j; i, j = i+1, j-1 {
			tokenInfoHistoryHeight[i], tokenInfoHistoryHeight[j] = tokenInfoHistoryHeight[j], tokenInfoHistoryHeight[i]
		}
		tokenInfoHistory = tokenInfoHistoryHeight
	}

	total = len(tokenInfoHistory)
	if start >= total {
		return total, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	count := 0
	for idx := total - start - 1; idx >= 0; idx-- {
		historyIdx := tokenInfoHistory[idx]
		buf := model.GSwap.HistoryData[historyIdx]
		history := &swapModel.BRC20History{}
		history.Unmarshal(buf)

		if height > 0 && height != int(history.Height) {
			continue
		}

		if count >= size {
			break
		}
		count += 1

		addressFrom, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptFrom), conf.GlobalNetParams)
		if err != nil {
			addressFrom = hex.EncodeToString([]byte(history.PkScriptFrom))
		}

		addressTo, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptTo), conf.GlobalNetParams)
		if err != nil {
			addressTo = hex.EncodeToString([]byte(history.PkScriptTo))
		}

		blockhash := ""
		if int(history.Height) < len(model.GlobalBlocksHash) {
			blockhash = utils.GetReversedStringHex(model.GlobalBlocksHash[history.Height])
		}
		nftsRsp = append(nftsRsp, &model.BRC20TickerHistoryInfo{
			Ticker: tokenInfo.Deploy.Data.BRC20Tick,
			Type:   constant.BRC20_HISTORY_TYPE_NAMES[history.Type],
			Valid:  history.Valid,

			InscriptionNumber: history.Inscription.InscriptionNumber,
			InscriptionId:     history.Inscription.InscriptionId,
			TxIdHex:           utils.GetReversedStringHex(history.TxId),
			Vout:              history.Vout,
			Offset:            history.Offset,
			Idx:               history.Idx,

			AddressFrom:         addressFrom,
			AddressTo:           addressTo,
			Satoshi:             history.Satoshi,
			Fee:                 history.Fee,
			Amount:              history.Amount,
			OverallBalance:      history.OverallBalance,
			TransferableBalance: history.TransferableBalance,
			AvailableBalance:    history.AvailableBalance,

			Height:       history.Height,
			TxIdx:        history.TxIdx,
			BlockHashHex: blockhash,
			BlockTime:    history.BlockTime,
		})
	}

	return total, nftsRsp, nil
}

func GetBRC20SummaryByAddress(pkScript []byte, start, size int) (total int, nftsRsp []*model.BRC20TokenSummaryInfo, err error) {
	logger.Log.Info("GetBRC20SummaryByAddress",
		zap.Int("start", start),
		zap.Int("size", size))

	var tokenBalance []*brc20Model.BRC20TokenBalance

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	userTokens, ok := model.GSwap.UserTokensBalanceData[string(pkScript)]
	if !ok {
		return 0, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	total = len(userTokens)
	if start >= total {
		return total, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	for _, balance := range userTokens {
		tokenBalance = append(tokenBalance, balance)
	}

	sort.Slice(tokenBalance, func(i, j int) bool {
		return strings.Compare(tokenBalance[i].Ticker, tokenBalance[j].Ticker) > 0
	})

	sort.SliceStable(tokenBalance, func(i, j int) bool {
		return tokenBalance[i].OverallBalance().CmpAlign(tokenBalance[j].OverallBalance()) > 0
	})

	total = len(tokenBalance)
	if start >= total {
		return total, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	for idx, balance := range tokenBalance[start:] {
		if idx >= size {
			break
		}

		uniqueLowerTicker := strings.ToLower(balance.Ticker)
		tokenInfo, ok := model.GSwap.InscriptionsTickerInfoMap[uniqueLowerTicker]
		if !ok {
			continue
		}

		nftsRsp = append(nftsRsp, &model.BRC20TokenSummaryInfo{
			Ticker:                 balance.Ticker,
			OverallBalance:         balance.OverallBalance().String(),
			TransferableBalance:    balance.TransferableBalance.String(),
			AvailableBalance:       balance.AvailableBalance.String(),
			AvailableBalanceSafe:   balance.AvailableBalanceSafe.String(),
			AvailableBalanceUnSafe: balance.AvailableBalance.Sub(balance.AvailableBalanceSafe).String(),
			Decimal:                int(tokenInfo.Deploy.Decimal),
		})
	}

	return total, nftsRsp, nil
}

func GetBRC20SummaryByAddressAndHeight(pkScript []byte, height, start, size int) (total int, nftsRsp []*model.BRC20TokenSummaryInfo, err error) {
	logger.Log.Info("GetBRC20SummaryByAddressAndHeight",
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	var tokenBalance []*swapModel.BRC20TokenBalance

	userTokens, ok := model.GSwap.UserTokensBalanceData[string(pkScript)]
	if !ok {
		return 0, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	total = len(userTokens)
	if start >= total {
		return total, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	for _, balance := range userTokens {
		tokenBalance = append(tokenBalance, balance)
	}

	sort.Slice(tokenBalance, func(i, j int) bool {
		return strings.Compare(tokenBalance[i].Ticker, tokenBalance[j].Ticker) > 0
	})

	total = len(tokenBalance)
	if start >= total {
		return total, make([]*model.BRC20TokenSummaryInfo, 0), nil
	}

	for idx, balance := range tokenBalance[start:] {
		if idx >= size {
			break
		}

		uniqueLowerTicker := strings.ToLower(balance.Ticker)
		tokenInfo, ok := model.GSwap.InscriptionsTickerInfoMap[uniqueLowerTicker]
		if !ok {
			continue
		}

		var lastHistory *swapModel.BRC20History
		for _, historyIdx := range balance.History {

			buf := model.GSwap.HistoryData[historyIdx] // fixme
			history := &swapModel.BRC20History{}
			history.Unmarshal(buf)

			if height < int(history.Height) {
				break
			}
			lastHistory = history
		}

		info := &model.BRC20TokenSummaryInfo{
			Ticker:              balance.Ticker,
			OverallBalance:      "0",
			TransferableBalance: "0",
			AvailableBalance:    "0",
			Decimal:             int(tokenInfo.Deploy.Decimal),
		}

		if lastHistory != nil {
			info.TransferableBalance = lastHistory.TransferableBalance
			info.AvailableBalance = lastHistory.AvailableBalance
			info.OverallBalance = lastHistory.OverallBalance
		}
		nftsRsp = append(nftsRsp, info)
	}

	return total, nftsRsp, nil
}

func GetBRC20TickerHistoryByAddress(pkScript []byte, historyType, ticker string, start, size int) (total int, nftsRsp []*model.BRC20TickerHistoryInfo, err error) {
	logger.Log.Info("GetBRC20TickerHistoryByAddress",
		zap.String("ticker", ticker),
		zap.String("type", historyType),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	userTokens, ok := model.GSwap.UserTokensBalanceData[string(pkScript)]
	if !ok {
		return 0, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	tokenInfo, ok := userTokens[ticker]
	if !ok {
		return 0, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	var tokenInfoHistory []uint32 = tokenInfo.History
	if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_MINT {
		tokenInfoHistory = tokenInfo.HistoryMint
	} else if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_TRANSFER {
		tokenInfoHistory = tokenInfo.HistoryInscribeTransfer
	} else if historyType == constant.BRC20_HISTORY_TYPE_SEND {
		tokenInfoHistory = tokenInfo.HistorySend
	} else if historyType == constant.BRC20_HISTORY_TYPE_RECEIVE {
		tokenInfoHistory = tokenInfo.HistoryReceive
	}

	total = len(tokenInfoHistory)
	if start >= total {
		return total, make([]*model.BRC20TickerHistoryInfo, 0), nil
	}

	count := 0
	for idx := len(tokenInfoHistory) - start - 1; idx >= 0; idx-- {
		if count >= size {
			break
		}
		count += 1

		historyIdx := tokenInfoHistory[idx]
		buf := model.GSwap.HistoryData[historyIdx]
		history := &swapModel.BRC20History{}
		history.Unmarshal(buf)

		addressFrom, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptFrom), conf.GlobalNetParams)
		if err != nil {
			addressFrom = hex.EncodeToString([]byte(history.PkScriptFrom))
		}

		addressTo, err := brc20Utils.GetAddressFromScript([]byte(history.PkScriptTo), conf.GlobalNetParams)
		if err != nil {
			addressTo = hex.EncodeToString([]byte(history.PkScriptTo))
		}

		blockhash := ""
		if int(history.Height) < len(model.GlobalBlocksHash) {
			blockhash = utils.GetReversedStringHex(model.GlobalBlocksHash[history.Height])
		}
		nftsRsp = append(nftsRsp, &model.BRC20TickerHistoryInfo{
			Ticker: tokenInfo.Ticker,
			Type:   constant.BRC20_HISTORY_TYPE_NAMES[history.Type],
			Valid:  history.Valid,

			InscriptionNumber: history.Inscription.InscriptionNumber,
			InscriptionId:     history.Inscription.InscriptionId,
			TxIdHex:           utils.GetReversedStringHex(history.TxId),
			Vout:              history.Vout,
			Offset:            history.Offset,
			Idx:               history.Idx,

			AddressFrom:         addressFrom,
			AddressTo:           addressTo,
			Satoshi:             history.Satoshi,
			Fee:                 history.Fee,
			Amount:              history.Amount,
			OverallBalance:      history.OverallBalance,
			TransferableBalance: history.TransferableBalance,
			AvailableBalance:    history.AvailableBalance,

			Height:       history.Height,
			TxIdx:        history.TxIdx,
			BlockHashHex: blockhash,
			BlockTime:    history.BlockTime,
		})
	}

	return total, nftsRsp, nil
}

func GetBRC20TickerInfoByAddress(pkScript []byte, ticker string) (nftRsp *model.BRC20TickerStatusInfoOfAddressResp, err error) {
	logger.Log.Info("GetBRC20TickerInfoByAddress",
		zap.String("ticker", ticker),
	)

	if model.GSwap == nil {
		return nil, errors.New("brc20 not ready")
	}

	validTransfer := make([]*brc20Model.InscriptionBRC20TickInfo, 0)
	validTransferResp := make([]*brc20Model.InscriptionBRC20TickInfoResp, 0)
	historyInscriptions := make([]brc20Model.InscriptionBRC20TickInfoResp, 0)
	nftRsp = &model.BRC20TickerStatusInfoOfAddressResp{
		Ticker:              ticker,
		OverallBalance:      "0",
		TransferableBalance: "0",
		AvailableBalance:    "0",

		AvailableBalanceSafe:   "0",
		AvailableBalanceUnSafe: "0",

		TransferableCount:        0,
		TransferableInscriptions: validTransferResp,

		HistoryCount:        0,
		HistoryInscriptions: historyInscriptions,
	}

	userTokens, ok := model.GSwap.UserTokensBalanceData[string(pkScript)]
	if !ok {
		return nftRsp, nil
	}

	tokenInfo, ok := userTokens[ticker]
	if !ok {
		return nftRsp, nil
	}

	for _, tr := range tokenInfo.ValidTransferMap {
		validTransfer = append(validTransfer, tr)
	}
	// sort by deploy height
	sort.Slice(validTransfer, func(i, j int) bool {
		idxi := uint64(validTransfer[i].Height)*4294967296 + uint64(validTransfer[i].TxIdx)
		idxj := uint64(validTransfer[j].Height)*4294967296 + uint64(validTransfer[j].TxIdx)
		return idxi > idxj
	})
	for idx, tr := range validTransfer {
		if idx > 7 {
			break
		}
		validTransferResp = append(validTransferResp, &brc20Model.InscriptionBRC20TickInfoResp{
			Height:            tr.Height,
			Data:              tr.Data,
			InscriptionNumber: tr.InscriptionNumber,
			InscriptionId:     tr.GetInscriptionId(),
			Satoshi:           tr.Satoshi,
		})
	}

	var tokenInfoHistory []uint32 = tokenInfo.History
	count := 0
	for idx := len(tokenInfoHistory) - 1; idx >= 0; idx-- {
		historyIdx := tokenInfoHistory[idx]
		buf := model.GSwap.HistoryData[historyIdx]
		history := &swapModel.BRC20History{}
		history.Unmarshal(buf)

		if !history.Valid {
			continue
		}
		if constant.BRC20_HISTORY_TYPE_N_INSCRIBE_TRANSFER == history.Type ||
			constant.BRC20_HISTORY_TYPE_N_SEND == history.Type {
			continue
		}
		if count >= 7 {
			break
		}
		count += 1

		historyInscriptions = append(historyInscriptions, history.Inscription)
	}

	nftRsp.Ticker = tokenInfo.Ticker
	nftRsp.OverallBalance = tokenInfo.OverallBalance().String()
	nftRsp.TransferableBalance = tokenInfo.TransferableBalance.String()
	nftRsp.AvailableBalance = tokenInfo.AvailableBalance.String()
	nftRsp.TransferableCount = len(tokenInfo.ValidTransferMap)
	nftRsp.TransferableInscriptions = validTransferResp
	nftRsp.HistoryCount = len(tokenInfoHistory)
	nftRsp.HistoryInscriptions = historyInscriptions
	nftRsp.AvailableBalanceSafe = tokenInfo.AvailableBalanceSafe.String()
	nftRsp.AvailableBalanceUnSafe = tokenInfo.AvailableBalance.Sub(tokenInfo.AvailableBalanceSafe).String()
	return nftRsp, nil
}

func GetBRC20TickerTransferableInscriptionsByAddress(pkScript []byte, ticker string, start, size int, isInvalid bool) (total int, nftsRsp []*brc20Model.InscriptionBRC20TickInfoResp, err error) {
	logger.Log.Info("GetBRC20TickerTransferableInscriptionsByAddress",
		zap.String("ticker", ticker),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	userTokens, ok := model.GSwap.UserTokensBalanceData[string(pkScript)]
	if !ok {
		return 0, make([]*brc20Model.InscriptionBRC20TickInfoResp, 0), nil
	}

	tokenInfo, ok := userTokens[ticker]
	if !ok {
		return 0, make([]*brc20Model.InscriptionBRC20TickInfoResp, 0), nil
	}

	var transferInscriptions []*brc20Model.InscriptionBRC20TickInfo

	if isInvalid {
		// for _, tr := range tokenInfo.InvalidTransferList {
		// 	transferInscriptions = append(transferInscriptions, tr)
		// }
	} else {
		for _, tr := range tokenInfo.ValidTransferMap {
			transferInscriptions = append(transferInscriptions, tr)
		}
	}
	// sort by deploy height
	sort.Slice(transferInscriptions, func(i, j int) bool {
		idxi := uint64(transferInscriptions[i].Height)*4294967296 + uint64(transferInscriptions[i].TxIdx)
		idxj := uint64(transferInscriptions[j].Height)*4294967296 + uint64(transferInscriptions[j].TxIdx)
		return idxi > idxj
	})

	total = len(transferInscriptions)
	if start >= total {
		return total, make([]*brc20Model.InscriptionBRC20TickInfoResp, 0), nil
	}

	for idx, inscription := range transferInscriptions[start:] {
		if idx >= size {
			break
		}
		nftsRsp = append(nftsRsp, &brc20Model.InscriptionBRC20TickInfoResp{
			Height:            inscription.Height,
			Data:              inscription.Data,
			InscriptionNumber: inscription.InscriptionNumber,
			InscriptionId:     fmt.Sprintf("%si%d", utils.GetReversedStringHex(inscription.TxId), inscription.Idx),
			Satoshi:           inscription.Satoshi,
		})
	}
	return total, nftsRsp, nil
}
