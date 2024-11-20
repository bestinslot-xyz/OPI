package loader

import (
	"encoding/hex"
	"fmt"
	"log"
	"os"
	"sort"
	"strings"

	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/constant"
	"github.com/unisat-wallet/libbrc20-indexer/decimal"
	"github.com/unisat-wallet/libbrc20-indexer/model"
	"github.com/unisat-wallet/libbrc20-indexer/utils"
)

func DumpBRC20InputData(fname string, brc20Datas chan interface{}, hexBody bool) {
	file, err := os.OpenFile(fname, os.O_RDWR|os.O_CREATE|os.O_TRUNC, 0777)
	if err != nil {
		log.Fatalf("open block index file failed, %s", err)
		return
	}
	defer file.Close()

	for dataIn := range brc20Datas {
		data := dataIn.(*model.InscriptionBRC20Data)

		var body, address string
		if hexBody {
			body = hex.EncodeToString(data.ContentBody)
			address = hex.EncodeToString([]byte(data.PkScript))
		} else {
			body = strings.ReplaceAll(string(data.ContentBody), "\n", " ")
			address, err = utils.GetAddressFromScript([]byte(data.PkScript), conf.GlobalNetParams)
			if err != nil {
				address = hex.EncodeToString([]byte(data.PkScript))
			}
		}

		fmt.Fprintf(file, "%d %s %d %d %d %d %s %d %s %s %d %d %d\n",
			data.Sequence,
			utils.HashString([]byte(data.TxId)),
			data.Idx,
			data.Vout,
			data.Offset,
			data.Satoshi,
			address,
			data.InscriptionNumber,
			body,
			data.CreateIdxKey,
			data.Height,
			data.TxIdx,
			data.BlockTime,
		)
	}
}

func DumpTickerInfoMap(fname string,
	historyData [][]byte,
	inscriptionsTickerInfoMap map[string]*model.BRC20TokenInfo,
	userTokensBalanceData map[string]map[string]*model.BRC20TokenBalance,
	tokenUsersBalanceData map[string]map[string]*model.BRC20TokenBalance,
) {

	file, err := os.OpenFile(fname, os.O_RDWR|os.O_CREATE|os.O_TRUNC, 0777)
	if err != nil {
		log.Fatalf("open block index file failed, %s", err)
		return
	}
	defer file.Close()

	var allTickers []string
	for ticker := range inscriptionsTickerInfoMap {
		allTickers = append(allTickers, ticker)
	}
	sort.SliceStable(allTickers, func(i, j int) bool {
		return allTickers[i] < allTickers[j]
	})

	for _, ticker := range allTickers {
		info := inscriptionsTickerInfoMap[ticker]
		nValid := 0
		for _, hIdx := range info.History {
			buf := historyData[hIdx]
			h := &model.BRC20History{}
			h.Unmarshal(buf)

			if h.Valid {
				nValid++
			}
		}

		fmt.Fprintf(file, "%s history: %d, valid: %d, minted: %s, holders: %d\n",
			info.Ticker,
			len(info.History),
			nValid,
			info.Deploy.TotalMinted.String(),
			len(tokenUsersBalanceData[ticker]),
		)

		// history
		for _, hIdx := range info.History {
			buf := historyData[hIdx]
			h := &model.BRC20History{}
			h.Unmarshal(buf)

			if !h.Valid {
				continue
			}

			addressFrom, err := utils.GetAddressFromScript([]byte(h.PkScriptFrom), conf.GlobalNetParams)
			if err != nil {
				addressFrom = hex.EncodeToString([]byte(h.PkScriptFrom))
			}

			addressTo, err := utils.GetAddressFromScript([]byte(h.PkScriptTo), conf.GlobalNetParams)
			if err != nil {
				addressTo = hex.EncodeToString([]byte(h.PkScriptTo))
			}

			fmt.Fprintf(file, "%s %s %s %s %s -> %s\n",
				info.Ticker,
				utils.HashString([]byte(h.TxId)),
				constant.BRC20_HISTORY_TYPE_NAMES[h.Type],
				h.Amount,
				addressFrom,
				addressTo,
			)
		}

		// holders
		var allHoldersPkScript []string
		for holder := range tokenUsersBalanceData[ticker] {
			allHoldersPkScript = append(allHoldersPkScript, holder)
		}
		// sort by holder address
		sort.SliceStable(allHoldersPkScript, func(i, j int) bool {
			return allHoldersPkScript[i] < allHoldersPkScript[j]
		})

		// holders
		for _, holder := range allHoldersPkScript {
			balanceData := tokenUsersBalanceData[ticker][holder]

			address, err := utils.GetAddressFromScript([]byte(balanceData.PkScript), conf.GlobalNetParams)
			if err != nil {
				address = hex.EncodeToString([]byte(balanceData.PkScript))
			}
			fmt.Fprintf(file, "%s %s history: %d, transfer: %d, balance: %s, tokens: %d\n",
				info.Ticker,
				address,
				len(balanceData.History),
				len(balanceData.ValidTransferMap),
				balanceData.OverallBalance().String(),
				len(userTokensBalanceData[holder]),
			)
		}
	}
}

func DumpModuleInfoMap(fname string,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo,
) {
	file, err := os.OpenFile(fname, os.O_RDWR|os.O_CREATE|os.O_TRUNC, 0777)
	if err != nil {
		log.Fatalf("open module dump file failed, %s", err)
		return
	}
	defer file.Close()

	var allModules []string
	for moduleId := range modulesInfoMap {
		allModules = append(allModules, moduleId)
	}
	sort.SliceStable(allModules, func(i, j int) bool {
		return allModules[i] < allModules[j]
	})

	for _, moduleId := range allModules {
		info := modulesInfoMap[moduleId]
		nValid := 0
		for _, h := range info.History {
			if h.Valid {
				nValid++
			}
		}

		fmt.Fprintf(file, "module %s(%s) nHistory: %d, nValidHistory: %d, nCommit: %d, nTickers: %d, nHolders: %d, swap: %d, lpholders: %d\n",
			info.Name,
			info.ID,
			len(info.History),
			nValid,
			len(info.CommitIdChainMap),
			len(info.TokenUsersBalanceDataMap),
			len(info.UsersTokenBalanceDataMap),

			len(info.LPTokenUsersBalanceMap),
			len(info.UsersLPTokenBalanceMap),
		)

		DumpModuleTickInfoMap(file, info.ConditionalApproveStateBalanceDataMap, info.TokenUsersBalanceDataMap, info.UsersTokenBalanceDataMap)

		DumpModuleSwapInfoMap(file, info.SwapPoolTotalBalanceDataMap, info.LPTokenUsersBalanceMap, info.UsersLPTokenBalanceMap)
	}
}

func DumpModuleTickInfoMap(file *os.File, condStateBalanceDataMap map[string]*model.BRC20ModuleConditionalApproveStateBalance,
	inscriptionsTickerInfoMap, userTokensBalanceData map[string]map[string]*model.BRC20ModuleTokenBalance,
) {

	var allTickers []string
	for ticker := range inscriptionsTickerInfoMap {
		allTickers = append(allTickers, ticker)
	}
	sort.SliceStable(allTickers, func(i, j int) bool {
		return allTickers[i] < allTickers[j]
	})

	for _, ticker := range allTickers {
		holdersMap := inscriptionsTickerInfoMap[ticker]

		nHistory := 0
		nValid := 0

		var allHoldersPkScript []string
		for holder, data := range holdersMap {
			nHistory += len(data.History)
			for _, h := range data.History {
				if h.Valid {
					nValid++
				}
			}
			allHoldersPkScript = append(allHoldersPkScript, holder)
		}
		sort.SliceStable(allHoldersPkScript, func(i, j int) bool {
			return allHoldersPkScript[i] < allHoldersPkScript[j]
		})

		fmt.Fprintf(file, " %s nHistory: %d, valid: %d, nHolders: %d\n",
			ticker,
			nHistory,
			nValid,
			// TokenTotalBalance[tick], // fixme
			len(holdersMap),
		)

		// holders
		for _, holder := range allHoldersPkScript {
			balanceData := holdersMap[holder]

			address, err := utils.GetAddressFromScript([]byte(balanceData.PkScript), conf.GlobalNetParams)
			if err != nil {
				address = hex.EncodeToString([]byte(balanceData.PkScript))
			}
			fmt.Fprintf(file, "  %s %s nHistory: %d, bnModule: %s, bnAvai: %s, bnSwap: %s, bnCond: %s, nToken: %d",
				ticker,
				address,
				len(balanceData.History),
				balanceData.ModuleBalance().String(),
				balanceData.AvailableBalance.String(),
				balanceData.SwapAccountBalance.String(),
				balanceData.CondApproveableBalance.String(),
				len(userTokensBalanceData[string(balanceData.PkScript)]),
			)

			if len(balanceData.ValidApproveMap) > 0 {
				fmt.Fprintf(file, ", nApprove: %d", len(balanceData.ValidApproveMap))
			}
			if len(balanceData.ReadyToWithdrawMap) > 0 {
				fmt.Fprintf(file, ", nWithdraw: %d", len(balanceData.ReadyToWithdrawMap))
			}
			fmt.Fprintf(file, "\n")
		}
	}

	fmt.Fprintf(file, "\n")

	// condStateBalanceDataMap
	for _, ticker := range allTickers {
		stateBalance, ok := condStateBalanceDataMap[ticker]
		if !ok {
			fmt.Fprintf(file, "  module deposit/withdraw state: %s - \n", ticker)
			continue
		}

		fmt.Fprintf(file, "  module deposit/withdraw state: %s deposit: %s, match: %s, new: %s, cancel: %s, wait: %s\n",
			ticker,
			stateBalance.BalanceDeposite.String(),
			stateBalance.BalanceApprove.String(),
			stateBalance.BalanceNewApprove.String(),
			stateBalance.BalanceCancelApprove.String(),

			stateBalance.BalanceNewApprove.Sub(
				stateBalance.BalanceApprove).Sub(
				stateBalance.BalanceCancelApprove).String(),
		)
	}

	fmt.Fprintf(file, "\n")
}

func DumpModuleSwapInfoMap(file *os.File,
	swapPoolTotalBalanceDataMap map[string]*model.BRC20ModulePoolTotalBalance,
	inscriptionsTickerInfoMap, userTokensBalanceData map[string]map[string]*decimal.Decimal) {

	var allTickers []string
	for ticker := range inscriptionsTickerInfoMap {
		allTickers = append(allTickers, ticker)
	}
	sort.SliceStable(allTickers, func(i, j int) bool {
		return allTickers[i] < allTickers[j]
	})

	for _, ticker := range allTickers {
		holdersMap := inscriptionsTickerInfoMap[ticker]

		var allHoldersPkScript []string
		for holder := range holdersMap {
			allHoldersPkScript = append(allHoldersPkScript, holder)
		}
		sort.SliceStable(allHoldersPkScript, func(i, j int) bool {
			return allHoldersPkScript[i] < allHoldersPkScript[j]
		})

		swap := swapPoolTotalBalanceDataMap[ticker]

		fmt.Fprintf(file, " pool: %s nHistory: %d, nLPholders: %d, lp: %s, %s: %s, %s: %s\n",
			ticker,
			len(swap.History),
			len(holdersMap),
			swap.LpBalance,
			swap.Tick[0],
			swap.TickBalance[0],
			swap.Tick[1],
			swap.TickBalance[1],
		)

		// holders
		for _, holder := range allHoldersPkScript {
			balanceData := holdersMap[holder]

			address, err := utils.GetAddressFromScript([]byte(holder), conf.GlobalNetParams)
			if err != nil {
				address = hex.EncodeToString([]byte(holder))
			}
			fmt.Fprintf(file, "  pool: %s %s lp: %s, swaps: %d\n",
				ticker,
				address,
				balanceData.String(),
				len(userTokensBalanceData[holder]),
			)
		}
	}
}
