package indexer

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"strings"

	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/constant"
	"github.com/unisat-wallet/libbrc20-indexer/decimal"
	"github.com/unisat-wallet/libbrc20-indexer/model"
	"github.com/unisat-wallet/libbrc20-indexer/utils"
)

func (g *BRC20ModuleIndexer) GetWithdrawInfoByKey(createIdxKey string) (
	withdrawInfo *model.InscriptionBRC20SwapInfo) {
	var ok bool
	// withdraw
	withdrawInfo, ok = g.InscriptionsWithdrawMap[createIdxKey]
	if !ok {
		withdrawInfo = nil
	}

	return withdrawInfo
}

func (g *BRC20ModuleIndexer) ProcessWithdraw(data *model.InscriptionBRC20Data, withdrawInfo *model.InscriptionBRC20SwapInfo) error {
	// ticker
	uniqueLowerTicker := strings.ToLower(withdrawInfo.Tick)
	tokenInfo, ok := g.InscriptionsTickerInfoMap[uniqueLowerTicker]
	if !ok {
		log.Printf("ProcessWithdraw send withdraw, but ticker invalid. txid: %s",
			utils.HashString([]byte(data.TxId)),
		)
		return errors.New("transfer, invalid ticker")
	}

	moduleInfo, ok := g.ModulesInfoMap[withdrawInfo.Module]
	if !ok {
		log.Printf("ProcessBRC20Withdraw send withdraw, but ticker invalid. txid: %s",
			hex.EncodeToString(utils.ReverseBytes([]byte(data.TxId))),
		)
		return errors.New("withdraw, module invalid")
	}

	var isInvalid bool

	// global history fixme
	// if g.EnableHistory {
	// 	historyObj := model.NewBRC20History(constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW, !isInvalid, true, withdrawInfo, nil, data)
	// 	history := g.UpdateHistoryHeightAndGetHistoryIndex(historyObj)

	// 	tokenInfo.History = append(tokenInfo.History, history)
	// 	// tokenInfo.HistoryWithdraw = append(tokenInfo.HistoryTransfer, history)
	// 	if !isInvalid {
	// 		// all history
	// 		g.AllHistory = append(g.AllHistory, history)
	// 	}
	// }

	// from
	// get user's tokens to update
	fromUserTokens, ok := moduleInfo.UsersTokenBalanceDataMap[string(withdrawInfo.Data.PkScript)]
	if !ok {
		log.Printf("ProcessBRC20Withdraw send from user missing. height: %d, txidx: %d",
			data.Height,
			data.TxIdx,
		)
		return errors.New("withdraw, send from user missing")
	}
	// get tokenBalance to update
	fromTokenBalance, ok := fromUserTokens[uniqueLowerTicker]
	if !ok {
		log.Printf("ProcessBRC20Withdraw send from ticker missing. height: %d, txidx: %d",
			data.Height,
			data.TxIdx,
		)
		return errors.New("withdraw, send from ticker missing")
	}

	// Cross-check whether the withdraw-inscription exists.
	if _, ok := fromTokenBalance.ReadyToWithdrawMap[data.CreateIdxKey]; !ok {
		log.Printf("ProcessBRC20Withdraw send from withdraw missing(dup withdraw?). height: %d, txidx: %d",
			data.Height,
			data.TxIdx,
		)
		return errors.New("withdraw, send from withdraw missing(dup)")
	}

	// available > amt
	balanceWithdraw := withdrawInfo.Amount
	fromTokenBalance.ReadyToWithdrawAmount = fromTokenBalance.ReadyToWithdrawAmount.Sub(balanceWithdraw)
	delete(fromTokenBalance.ReadyToWithdrawMap, data.CreateIdxKey)
	fromTokenBalance.UpdateHeight = data.Height

	if fromTokenBalance.AvailableBalance.Cmp(balanceWithdraw) < 0 { // invalid
		isInvalid = true
	}

	// to address
	receiverPkScript := string(data.PkScript)
	if data.Satoshi == 0 {
		receiverPkScript = string(withdrawInfo.Data.PkScript)
		data.PkScript = receiverPkScript
	}

	// global history
	historyData := &model.BRC20SwapHistoryWithdrawData{
		Tick:   withdrawInfo.Tick,
		Amount: withdrawInfo.Amount.String(),
	}
	history := model.NewBRC20ModuleHistory(true, constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW, withdrawInfo.Data, data, historyData, !isInvalid)
	moduleInfo.History = append(moduleInfo.History, history)
	if isInvalid {
		// from invalid history
		fromHistory := model.NewBRC20ModuleHistory(true, constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW_FROM, withdrawInfo.Data, data, nil, false)
		fromTokenBalance.History = append(fromTokenBalance.History, fromHistory)
		return errors.New("withdraw, insufficient available balance")
	}

	// set from
	// The available balance here needs to be directly deducted and transferred to WithdrawableBalance.
	fromTokenBalance.AvailableBalanceSafe = fromTokenBalance.AvailableBalanceSafe.Sub(balanceWithdraw)
	fromTokenBalance.AvailableBalance = fromTokenBalance.AvailableBalance.Sub(balanceWithdraw)

	fromHistory := model.NewBRC20ModuleHistory(true, constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW_FROM, withdrawInfo.Data, data, nil, true)
	fromTokenBalance.History = append(fromTokenBalance.History, fromHistory)

	// to
	tokenBalance := g.GetUserTokenBalance(withdrawInfo.Tick, receiverPkScript)
	// set to
	tokenBalance.UpdateHeight = data.Height

	if data.BlockTime > 0 {
		tokenBalance.AvailableBalanceSafe = tokenBalance.AvailableBalanceSafe.Add(withdrawInfo.Amount)
	}
	tokenBalance.AvailableBalance = tokenBalance.AvailableBalance.Add(withdrawInfo.Amount)

	// burn
	if len(receiverPkScript) == 1 && []byte(receiverPkScript)[0] == 0x6a {
		tokenInfo.Deploy.Burned = tokenInfo.Deploy.Burned.Add(withdrawInfo.Amount)
	}

	// fixme: add user module history
	// if g.EnableHistory {
	// 	historyObj := model.NewBRC20History(constant.BRC20_HISTORY_TYPE_N_RECEIVE, true, true, withdrawInfo, tokenBalance, data)
	// 	toHistory := g.UpdateHistoryHeightAndGetHistoryIndex(historyObj)

	// 	tokenBalance.History = append(tokenBalance.History, toHistory)
	// 	tokenBalance.HistoryReceive = append(tokenBalance.HistoryReceive, toHistory)

	// 	userHistoryTo := g.GetBRC20HistoryByUser(receiverPkScript)
	// 	userHistoryTo.History = append(userHistoryTo.History, toHistory)
	// }

	// toHistory := model.NewBRC20ModuleHistory(true, constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW_TO, withdrawInfo.Data, data, nil, true)
	// tokenBalance.History = append(tokenBalance.History, toHistory)

	////////////////////////////////////////////////////////////////
	// withdraw to a module, is NOT deposit
	return nil
}

func (g *BRC20ModuleIndexer) ProcessInscribeWithdraw(data *model.InscriptionBRC20Data) error {
	var body model.InscriptionBRC20ModuleWithdrawContent
	if err := json.Unmarshal(data.ContentBody, &body); err != nil {
		log.Printf("parse module withdraw json failed. txid: %s",
			hex.EncodeToString(utils.ReverseBytes([]byte(data.TxId))),
		)
		return err
	}

	// lower case only
	if body.Module != strings.ToLower(body.Module) {
		return errors.New("module id invalid")
	}

	moduleInfo, ok := g.ModulesInfoMap[body.Module]
	if !ok { // invalid module
		return errors.New("module invalid")
	}

	if data.Height < conf.ENABLE_SWAP_WITHDRAW_HEIGHT {
		return errors.New("module withdraw disable")
	}

	if len(body.Tick) != 4 && len(body.Tick) != 5 {
		return errors.New("tick invalid")
	}
	uniqueLowerTicker := strings.ToLower(body.Tick)
	tokenInfo, ok := g.InscriptionsTickerInfoMap[uniqueLowerTicker]
	if !ok {
		return errors.New("tick not exist")
	}
	tinfo := tokenInfo.Deploy

	amt, err := decimal.NewDecimalFromString(body.Amount, int(tinfo.Decimal))
	if err != nil {
		return errors.New(fmt.Sprintf("withdraw amount invalid: %s", body.Amount))
	}
	if amt.Sign() <= 0 || amt.Cmp(tinfo.Max) > 0 {
		return errors.New("amount out of range")
	}

	balanceWithdraw := decimal.NewDecimalCopy(amt)

	// Unify ticker case
	body.Tick = tokenInfo.Ticker
	// Set up withdraw data for subsequent use.
	withdrawInfo := &model.InscriptionBRC20SwapInfo{
		Data: data,
	}
	withdrawInfo.Module = body.Module
	withdrawInfo.Tick = tokenInfo.Ticker
	withdrawInfo.Amount = balanceWithdraw

	// global history
	historyData := &model.BRC20SwapHistoryWithdrawData{
		Tick:   withdrawInfo.Tick,
		Amount: withdrawInfo.Amount.String(),
	}
	history := model.NewBRC20ModuleHistory(false, constant.BRC20_HISTORY_MODULE_TYPE_N_INSCRIBE_WITHDRAW, data, data, historyData, true)
	moduleInfo.History = append(moduleInfo.History, history)

	// Check if the module balance is sufficient to withdraw
	moduleTokenBalance := moduleInfo.GetUserTokenBalance(withdrawInfo.Tick, data.PkScript)
	{

		moduleTokenBalance.ReadyToWithdrawAmount = moduleTokenBalance.ReadyToWithdrawAmount.Add(balanceWithdraw)

		history.Valid = true
		// Update personal withdraw lookup table ReadyToWithdrawMap
		if moduleTokenBalance.ReadyToWithdrawMap == nil {
			moduleTokenBalance.ReadyToWithdrawMap = make(map[string]*model.InscriptionBRC20Data, 1)
		}
		moduleTokenBalance.ReadyToWithdrawMap[data.CreateIdxKey] = data

		moduleTokenBalance.UpdateHeight = data.Height
		// Update global withdraw lookup table
		g.InscriptionsWithdrawMap[data.CreateIdxKey] = withdrawInfo
		// g.InscriptionsValidBRC20DataMap[data.CreateIdxKey] = withdrawInfo.Data  // fixme
	}

	return nil

}
