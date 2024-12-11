package brc20

import (
	"brc20query/lib/utils"
	"brc20query/logger"
	"brc20query/model"
	"encoding/hex"
	"errors"
	"strings"

	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/constant"
	swapIndexer "github.com/unisat-wallet/libbrc20-indexer/indexer"
	swapModel "github.com/unisat-wallet/libbrc20-indexer/model"
	swapUtils "github.com/unisat-wallet/libbrc20-indexer/utils"
	"go.uber.org/zap"
)

// for api, history
func GetBRC20ModuleHistory(historyType, module string, startHeight, endHeight, start, size int) (total int, nftsRsp []*model.BRC20ModuleHistoryInfo, err error) {
	logger.Log.Info("GetBRC20ModuleHistory",
		zap.String("type", historyType),
		zap.String("module", module),
		zap.Int("startHeight", startHeight),
		zap.Int("endHeight", endHeight),
		zap.Int("start", start),
		zap.Int("size", size))

	if model.GSwap == nil {
		return 0, nil, errors.New("brc20 not ready")
	}

	moduleInfo, ok := model.GSwap.ModulesInfoMap[module]
	if !ok {
		return 0, make([]*model.BRC20ModuleHistoryInfo, 0), nil
	}

	var moduleInfoHistory []*swapModel.BRC20ModuleHistory = moduleInfo.History
	if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_MINT {
		moduleInfoHistory = moduleInfo.History
	} else if historyType == constant.BRC20_HISTORY_TYPE_INSCRIBE_TRANSFER {
		moduleInfoHistory = moduleInfo.History
	} else if historyType == constant.BRC20_HISTORY_TYPE_TRANSFER {
		moduleInfoHistory = moduleInfo.History
	}

	if startHeight > 0 {
		var moduleInfoHistoryHeight []*swapModel.BRC20ModuleHistory
		for idx := len(moduleInfoHistory) - 1; idx >= 0; idx-- {
			history := moduleInfoHistory[idx]

			if endHeight > 0 && endHeight <= int(history.Height) {
				continue
			}

			if startHeight > int(history.Height) {
				break
			}
			moduleInfoHistoryHeight = append(moduleInfoHistoryHeight, history)
		}
		// reverse
		for i, j := 0, len(moduleInfoHistoryHeight)-1; i < j; i, j = i+1, j-1 {
			moduleInfoHistoryHeight[i], moduleInfoHistoryHeight[j] = moduleInfoHistoryHeight[j], moduleInfoHistoryHeight[i]
		}
		moduleInfoHistory = moduleInfoHistoryHeight
	}

	total = len(moduleInfoHistory)
	if start >= total {
		return total, make([]*model.BRC20ModuleHistoryInfo, 0), nil
	}

	count := 0
	for idx := start; idx < total; idx++ {
		history := moduleInfoHistory[idx]

		if count >= size {
			break
		}
		count += 1

		addressFrom, err := swapUtils.GetAddressFromScript([]byte(history.PkScriptFrom), conf.GlobalNetParams)
		if err != nil {
			addressFrom = hex.EncodeToString([]byte(history.PkScriptFrom))
		}

		addressTo, err := swapUtils.GetAddressFromScript([]byte(history.PkScriptTo), conf.GlobalNetParams)
		if err != nil {
			addressTo = hex.EncodeToString([]byte(history.PkScriptTo))
		}

		blockhash := ""
		if int(history.Height) < len(model.GlobalBlocksHash) {
			blockhash = utils.GetReversedStringHex(model.GlobalBlocksHash[history.Height])
		}

		historyResp := &model.BRC20ModuleHistoryInfo{
			Type:  constant.BRC20_HISTORY_TYPE_NAMES[history.Type],
			Valid: history.Valid,

			InscriptionNumber: history.Inscription.InscriptionNumber,
			InscriptionId:     history.Inscription.InscriptionId,
			TxIdHex:           utils.GetReversedStringHex(history.TxId),
			Vout:              history.Vout,
			Offset:            history.Offset,
			Idx:               history.Idx,
			ContentBody:       string(history.Inscription.ContentBody),

			AddressFrom: addressFrom,
			AddressTo:   addressTo,
			Satoshi:     history.Satoshi,

			Height:       history.Height,
			TxIdx:        history.TxIdx,
			BlockHashHex: blockhash,
			BlockTime:    history.BlockTime,

			Data: history.Data,
		}
		nftsRsp = append(nftsRsp, historyResp)
	}

	return total, nftsRsp, nil
}

func BRC20ModuleVerifySwapCommitContent(
	module string,
	commitsStr []string,
	commitsObj []*swapModel.InscriptionBRC20ModuleSwapCommitContent,
	results []*swapModel.SwapFunctionResultCheckState) (resp *model.BRC20ModuleVerifySwapCommitResp, err error) {

	if model.GSwap == nil {
		return nil, errors.New("swap not ready")
	}

	var pickUsersPkScript = make(map[string]bool, 0)
	var pickTokensTick = make(map[string]bool, 0)
	var pickPoolsPair = make(map[string]bool, 0)
	for _, r := range results {
		for _, user := range r.Users {
			if pkScript, err := utils.GetPkScriptByAddress(user.Address); err != nil {
				return nil, errors.New("result, addr invalid")
			} else {
				pickUsersPkScript[string(pkScript)] = true
			}
			if len(user.Tick) == 4 {
				tick := strings.ToLower(user.Tick)
				pickTokensTick[tick] = true
			}
		}

		for _, pool := range r.Pools {
			token0, token1, err := swapUtils.DecodeTokensFromSwapPair(pool.Pair)
			if err != nil {
				return nil, errors.New("result, pool pair invalid")
			}
			poolPair := swapIndexer.GetLowerInnerPairNameByToken(token0, token1)
			pickPoolsPair[poolPair] = true

			token0 = strings.ToLower(token0)
			token1 = strings.ToLower(token1)
			pickTokensTick[token0] = true
			pickTokensTick[token1] = true
		}
	}

	swapState := model.GSwap.CherryPick(module, pickUsersPkScript, pickTokensTick, pickPoolsPair)

	resp = &model.BRC20ModuleVerifySwapCommitResp{
		Valid: false,
	}

	total := len(commitsStr)
	if total >= 2 {
		swapState.BRC20ModulePrepareSwapCommitContent(commitsStr, commitsObj)
	}

	commitStr := commitsStr[total-1]
	commitObj := commitsObj[total-1]

	idx, critical, err := swapState.BRC20ModuleVerifySwapCommitContent(commitStr, commitObj, results)
	if err != nil {
		resp.Critical = critical
		resp.FunctionIndex = idx
		if idx >= 0 && idx < len(commitObj.Data) {
			resp.FunctionId = commitObj.Data[idx].ID
		}
		resp.Message = err.Error()
	} else {
		resp.Critical = critical
		resp.Valid = true
		resp.FunctionIndex = 0
		resp.FunctionId = ""
		resp.Message = "ok"
	}

	return resp, nil
}

func GetBRC20ModuleTickerInfoByAddress(pkScript []byte, module, ticker string) (nftRsp *model.BRC20ModuleTickerStatusInfoOfAddressResp, err error) {
	logger.Log.Info("GetBRC20ModuleTickerInfoByAddress",
		zap.String("ticker", ticker),
	)

	if model.GSwap == nil {
		return nil, errors.New("brc20 not ready")
	}

	nftRsp = &model.BRC20ModuleTickerStatusInfoOfAddressResp{
		Ticker: ticker,

		ModuleAccountBalance: "0",
		SwapAccountBalance:   "0",

		AvailableBalance:       "0",
		ApproveableBalance:     "0",
		CondApproveableBalance: "0",
		ReadyToWithdrawAmount:  "0",

		HistoryCount: 0,
	}

	moduleInfo, ok := model.GSwap.ModulesInfoMap[module]
	if !ok {
		return nftRsp, nil
	}

	holdersMap, ok := moduleInfo.TokenUsersBalanceDataMap[ticker]
	if !ok {
		return nftRsp, nil
	}

	tokenInfo, ok := holdersMap[string(pkScript)]
	if !ok {
		return nftRsp, nil
	}

	nftRsp.Ticker = tokenInfo.Tick
	nftRsp.ModuleAccountBalance = tokenInfo.ModuleBalance().String()
	nftRsp.SwapAccountBalance = tokenInfo.SwapAccountBalance.String()

	nftRsp.AvailableBalance = tokenInfo.AvailableBalance.String()
	nftRsp.CondApproveableBalance = tokenInfo.CondApproveableBalance.String()
	nftRsp.ApproveableBalance = tokenInfo.ApproveableBalance.String()
	nftRsp.ReadyToWithdrawAmount = tokenInfo.ReadyToWithdrawAmount.String()

	nftRsp.HistoryCount = 0
	return nftRsp, nil
}
