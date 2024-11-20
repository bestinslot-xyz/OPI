package indexer

import (
	"encoding/hex"
	"fmt"
	"log"
	"strings"
	"time"

	"go.uber.org/zap"

	"brc20query/logger"

	"github.com/unisat-wallet/libbrc20-indexer/constant"
	"github.com/unisat-wallet/libbrc20-indexer/decimal"
	"github.com/unisat-wallet/libbrc20-indexer/loader"
	"github.com/unisat-wallet/libbrc20-indexer/model"
)

// func (g *BRC20ModuleIndexer) SaveDataToDB(height int) {

func (g *BRC20ModuleIndexer) PurgeHistoricalData() {
	// purge history
	g.AllHistory = make([]uint32, 0) // fixme
	g.InscriptionsTransferRemoveMap = make(map[string]uint32, 0)
	g.InscriptionsApproveRemoveMap = make(map[string]uint32, 0)
	g.InscriptionsCondApproveRemoveMap = make(map[string]uint32, 0)
	g.InscriptionsCommitRemoveMap = make(map[string]uint32, 0)
}

func (g *BRC20ModuleIndexer) UpdateBRC20BlockEventsHash() {
	eventsPerBlock := []string{}
	for _, hIdx := range g.AllHistory {
		buf := g.HistoryData[hIdx]
		h := &model.BRC20History{}
		h.Unmarshal(buf)

		if !h.Valid {
			continue
		}

		pkScriptFrom := hex.EncodeToString([]byte(h.PkScriptFrom))
		pkScriptTo := hex.EncodeToString([]byte(h.PkScriptTo))

		inscription_id := h.Inscription.InscriptionId
		ticker := strings.ToLower(h.Inscription.Data.BRC20Tick)
		info := g.InscriptionsTickerInfoMap[ticker]

		var res string
		if h.Type == constant.BRC20_HISTORY_TYPE_N_INSCRIBE_DEPLOY {
			res = fmt.Sprintf("deploy-inscribe;%s;%s;%s;%s;%s;%d;%s;%b",
				inscription_id,
				pkScriptTo,
				ticker,
				info.Ticker,
				info.Deploy.Max.String(),
				info.Deploy.Decimal,
				info.Deploy.Limit.String(),
				info.Deploy.SelfMint,
			)

		} else if h.Type == constant.BRC20_HISTORY_TYPE_N_INSCRIBE_MINT {
			res = fmt.Sprintf("mint-inscribe;%s;%s;%s;%s;%s",
				inscription_id,
				pkScriptTo,
				ticker,
				info.Ticker,
				h.Amount,
				// res += event["parent_id"] // fixme
			)

		} else if h.Type == constant.BRC20_HISTORY_TYPE_N_INSCRIBE_TRANSFER {
			res = fmt.Sprintf("transfer-inscribe;%s;%s;%s;%s;%s",
				inscription_id,
				pkScriptTo,
				ticker,
				info.Ticker,
				h.Amount,
			)
		} else if h.Type == constant.BRC20_HISTORY_TYPE_N_TRANSFER {
			res = fmt.Sprintf("transfer-transfer;%s;%s;%s;%s;%s;%s",
				inscription_id,
				pkScriptFrom,
				pkScriptTo,
				ticker,
				info.Ticker,
				h.Amount,
			)
		} else if h.Type == constant.BRC20_HISTORY_MODULE_TYPE_N_WITHDRAW {
			res = fmt.Sprintf("withdraw-transfer;%s;%s;%s;%s;%s;%s",
				inscription_id,
				pkScriptFrom,
				pkScriptTo,
				ticker,
				info.Ticker,
				h.Amount,
			)
		}
		eventsPerBlock = append(eventsPerBlock, res)
	}

}
func (g *BRC20ModuleIndexer) SaveDataToDB(height uint32) {
	tx, err := loader.SwapDB.Begin()
	if err != nil {
		log.Panic("PG Begin Wrong: ", err)
	}
	defer tx.Rollback()

	brc20Tx, err := loader.BRC20DB.Begin()
	if err != nil {
		log.Panic("PG Begin Wrong: ", err)
	}
	defer brc20Tx.Rollback()

	// ticker info
	loader.SaveDataToDBTickerInfoMap(tx, height, g.InscriptionsTickerInfoMap)
	loader.SaveDataToDBTickerBalanceMap(tx, height, g.UserTokensBalanceData)
	// loader.SaveDataToDBTickerHistoryMap(tx, height, g.AllHistory) // fixme

	loader.SaveDataToDBTransferStateMap(tx, height, g.InscriptionsTransferRemoveMap)
	loader.SaveDataToDBValidTransferMap(tx, height, g.InscriptionsValidTransferMap)

	// module info
	loader.SaveDataToDBModuleInfoMap(tx, height, g.ModulesInfoMap)
	loader.SaveDataToDBModuleHistoryMap(tx, height, g.ModulesInfoMap)
	loader.SaveDataToDBModuleCommitChainMap(tx, height, g.ModulesInfoMap)
	loader.SaveDataToDBModuleUserBalanceMap(tx, height, g.ModulesInfoMap)
	loader.SaveDataToDBModulePoolLpBalanceMap(tx, height, g.ModulesInfoMap)
	loader.SaveDataToDBModuleUserLpBalanceMap(tx, height, g.ModulesInfoMap)

	loader.SaveDataToDBSwapApproveStateMap(tx, height, g.InscriptionsApproveRemoveMap)
	loader.SaveDataToDBSwapApproveMap(tx, height, g.InscriptionsValidApproveMap)

	loader.SaveDataToDBSwapCondApproveStateMap(tx, height, g.InscriptionsCondApproveRemoveMap)
	loader.SaveDataToDBSwapCondApproveMap(tx, height, g.InscriptionsValidConditionalApproveMap)

	loader.SaveDataToDBSwapCommitStateMap(tx, height, g.InscriptionsCommitRemoveMap)
	loader.SaveDataToDBSwapCommitMap(tx, height, g.InscriptionsValidCommitMap)

	loader.SaveDataToDBSwapWithdrawStateMap(tx, height, g.InscriptionsWithdrawRemoveMap)
	loader.SaveDataToDBSwapWithdrawMap(tx, height, g.InscriptionsWithdrawMap)

	loader.SaveDataToBRC20DBSwapWithdrawMap(brc20Tx, height, g.InscriptionsValidWithdrawMap)

	if err := tx.Commit(); err != nil {
		log.Panic("tx commit error: ", err)
	}
	if err := brc20Tx.Commit(); err != nil {
		log.Panic("brc20Tx commit error: ", err)
	}
}

func (g *BRC20ModuleIndexer) LoadDataFromDB(height int) {
	var (
		err error
		st  time.Time
	)

	st = time.Now()
	if g.InscriptionsTickerInfoMap, err = loader.LoadFromDbTickerInfoMap(); err != nil {
		log.Fatal("LoadFromDBTickerInfoMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBTickerInfoMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsTickerInfoMap)),
	)

	st = time.Now()
	if g.UserTokensBalanceData, err = loader.LoadFromDbUserTokensBalanceData(g.InscriptionsTickerInfoMap, nil, nil); err != nil {
		log.Fatal("LoadFromDBUserTokensBalanceData failed: ", err)
	}
	g.TokenUsersBalanceData = loader.UserTokensBalanceMap2TokenUsersBalanceMap(g.InscriptionsTickerInfoMap, g.UserTokensBalanceData)
	logger.Log.Info("LoadFromDBUserTokensBalanceData",
		zap.String("duration", time.Since(st).String()),
		zap.Int("ticks", len(g.TokenUsersBalanceData)),
		zap.Int("addresses", len(g.UserTokensBalanceData)),
	)

	// st = time.Now()
	// if g.InscriptionsTransferRemoveMap, err = loader.LoadFromDBTransferStateMap(); err != nil {
	// 	log.Fatal("LoadFromDBTransferStateMap failed: ", err)
	// }
	// logger.Log.Info("LoadFromDBTransferStateMap",
	// 	zap.String("duration", time.Since(st).String()),
	// 	zap.Int("count", len(g.InscriptionsTransferRemoveMap)),
	// )

	st = time.Now()
	if g.InscriptionsValidTransferMap, err = loader.LoadFromDBValidTransferMap(g.InscriptionsTickerInfoMap); err != nil {
		log.Fatal("LoadFromDBvalidTransferMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBvalidTransferMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsValidTransferMap)),
	)

	// update user valid TransferMap
	for k, v := range g.InscriptionsValidTransferMap {
		ticker := strings.ToLower(v.Tick)
		g.UserTokensBalanceData[v.PkScript][ticker].ValidTransferMap[k] = v
	}

	st = time.Now()
	if g.ModulesInfoMap, err = loader.LoadFromDBModuleInfoMap(); err != nil {
		log.Fatal("LoadFromDBModuleInfoMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBModuleInfoMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.ModulesInfoMap)),
	)

	// st = time.Now()
	// if g.InscriptionsApproveRemoveMap, err = loader.LoadFromDBSwapApproveStateMap(nil); err != nil {
	// 	log.Fatal("LoadFromDBSwapApproveStateMap failed: ", err)
	// }
	// logger.Log.Info("LoadFromDBSwapApproveStateMap",
	// 	zap.String("duration", time.Since(st).String()),
	// 	zap.Int("count", len(g.InscriptionsApproveRemoveMap)),
	// )

	st = time.Now()
	if g.InscriptionsValidApproveMap, err = loader.LoadFromDBSwapApproveMap(nil); err != nil {
		log.Fatal("LoadFromDBSwapApproveMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBSwapApproveMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsValidApproveMap)),
	)

	// st = time.Now()
	// if g.InscriptionsCondApproveRemoveMap, err = loader.LoadFromDBSwapCondApproveStateMap(nil); err != nil {
	// 	log.Fatal("LoadFromDBSwapCondApproveStateMap failed: ", err)
	// }
	// logger.Log.Info("LoadFromDBSwapCondApproveStateMap",
	// 	zap.String("duration", time.Since(st).String()),
	// 	zap.Int("count", len(g.InscriptionsCondApproveRemoveMap)),
	// )

	st = time.Now()
	if g.InscriptionsValidConditionalApproveMap, err = loader.LoadFromDBSwapCondApproveMap(nil); err != nil {
		log.Fatal("LoadFromDBSwapCondApproveMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBSwapCondApproveMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsValidConditionalApproveMap)),
	)

	// st = time.Now()
	// if g.InscriptionsCommitRemoveMap, err = loader.LoadFromDBSwapCommitStateMap(nil); err != nil {
	// 	log.Fatal("LoadFromDBSwapCommitStateMap failed: ", err)
	// }
	// logger.Log.Info("LoadFromDBSwapCommitStateMap",
	// 	zap.String("duration", time.Since(st).String()),
	// 	zap.Int("count", len(g.InscriptionsCommitRemoveMap)),
	// )

	st = time.Now()
	if g.InscriptionsValidCommitMap, err = loader.LoadFromDBSwapCommitMap(nil); err != nil {
		log.Fatal("LoadFromDBSwapCommitMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBSwapCommitMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsValidCommitMap)),
	)

	// st = time.Now()
	// if g.InscriptionsWithdrawRemoveMap, err = loader.LoadFromDBSwapWithdrawStateMap(nil); err != nil {
	// 	log.Fatal("LoadFromDBSwapWithdrawStateMap failed: ", err)
	// }
	// logger.Log.Info("LoadFromDBSwapWithdrawStateMap",
	//    zap.String("duration", time.Since(st).String()),
	// 	zap.Int("count", len(g.InscriptionsWithdrawRemoveMap)),
	// )

	st = time.Now()
	if g.InscriptionsWithdrawMap, err = loader.LoadFromDBSwapWithdrawMap(nil); err != nil {
		log.Fatal("LoadFromDBSwapWithdrawMap failed: ", err)
	}
	logger.Log.Info("LoadFromDBSwapWithdrawMap",
		zap.String("duration", time.Since(st).String()),
		zap.Int("count", len(g.InscriptionsWithdrawMap)),
	)

	for mid, info := range g.ModulesInfoMap {
		logger.Log.Info("loadFromDBSwapModuleInfo",
			zap.String("moduleId", mid),
		)
		loadFromDBSwapModuleInfo(mid, info)
	}
}

func loadFromDBSwapModuleInfo(mid string, info *model.BRC20ModuleSwapInfo) {
	var st = time.Now()
	if hm, err := loader.LoadFromDBModuleHistoryMap(mid); err != nil {
		log.Fatal("LoadFromDBModuleHistoryMap failed: ", err)
	} else {
		logger.Log.Info("LoadFromDBModuleHistoryMap",
			zap.String("duration", time.Since(st).String()),
			zap.Int("count", len(hm)),
		)
		for _, history := range hm {
			info.History = history
		}
	}

	st = time.Now()
	if ccs, err := loader.LoadModuleCommitChain(mid, nil); err != nil {
		log.Fatal("LoadModuleCommitChain failed: ", err)
	} else {
		logger.Log.Info("LoadModuleCommitChain",
			zap.String("duration", time.Since(st).String()),
			zap.Int("count", len(ccs)),
		)
		for _, cc := range ccs {
			if cc.Valid && cc.Connected {
				info.CommitIdChainMap[cc.CommitID] = struct{}{}
			} else if cc.Valid && !cc.Connected {
				info.CommitIdMap[cc.CommitID] = struct{}{}
			} else {
				info.CommitInvalidMap[cc.CommitID] = struct{}{}
			}
		}
	}

	// [tick][address]balanceData
	st = time.Now()
	if atbm, err := loader.LoadFromDBModuleUserBalanceMap(mid, nil, nil); err != nil {
		log.Fatal("LoadFromDBModuleUserBalanceMap failed: ", err)
	} else {
		info.UsersTokenBalanceDataMap = atbm
		info.TokenUsersBalanceDataMap = make(map[string]map[string]*model.BRC20ModuleTokenBalance)
		for addr, tbs := range atbm {
			for tick, balance := range tbs {
				if _, ok := info.TokenUsersBalanceDataMap[tick]; !ok {
					info.TokenUsersBalanceDataMap[tick] = make(map[string]*model.BRC20ModuleTokenBalance)
				}

				// [tick][address]balanceData
				info.TokenUsersBalanceDataMap[tick][addr] = balance
			}
		}

		logger.Log.Info("LoadFromDBModuleUserBalanceMap",
			zap.String("duration", time.Since(st).String()),
			zap.Int("count", len(atbm)),
			zap.Int("addresses", len(info.UsersTokenBalanceDataMap)),
		)
	}

	st = time.Now()
	if poolBalanceMap, err := loader.LoadFromDBModulePoolLpBalanceMap(mid, nil); err != nil {
		log.Fatal("LoadFromDBModulePoolLpBalanceMap failed: ", err)
	} else {
		logger.Log.Info("LoadFromDBModulePoolLpBalanceMap",
			zap.String("duration", time.Since(st).String()),
			zap.Int("count", len(poolBalanceMap)),
		)
		info.SwapPoolTotalBalanceDataMap = poolBalanceMap
	}

	// [pool][address]balance
	st = time.Now()
	if userLpBalanceMap, err := loader.LoadFromDBModuleUserLpBalanceMap(mid, nil, nil); err != nil {
		log.Fatal("LoadFromDBModuleUserLpBalanceMap failed: ", err)
	} else {
		info.UsersLPTokenBalanceMap = userLpBalanceMap

		for addr, pbs := range userLpBalanceMap {
			for pool, balance := range pbs {
				if _, ok := info.LPTokenUsersBalanceMap[pool]; !ok {
					info.LPTokenUsersBalanceMap[pool] = make(map[string]*decimal.Decimal)
				}
				// [address][pool]balance
				info.LPTokenUsersBalanceMap[pool][addr] = balance
			}
		}

		logger.Log.Info("LoadFromDBModuleUserLpBalanceMap",
			zap.String("duration", time.Since(st).String()),
			zap.Int("count", len(userLpBalanceMap)),
			zap.Int("addresses", len(info.UsersLPTokenBalanceMap)),
		)
	}

}
