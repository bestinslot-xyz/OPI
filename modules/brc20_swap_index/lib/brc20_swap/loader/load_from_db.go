package loader

import (
	"database/sql"
	"fmt"
	"strings"

	"github.com/lib/pq"
	"github.com/unisat-wallet/libbrc20-indexer/decimal"
	"github.com/unisat-wallet/libbrc20-indexer/model"
)

// buildSQLWhereInStr
// colValsPair: [[val1, val2, "colume"], ...]
func buildSQLWhereInStr(colValsPair [][]string, startIndex ...int) (conds []string, args []any) {
	conds = make([]string, 0)
	args = make([]any, 0)
	argIndex := 1
	if len(startIndex) > 0 {
		argIndex = startIndex[0]
		if argIndex < 1 {
			panic("argIndex must be greater than 0")
		}
	}

	// build ordered condition
	for _, pair := range colValsPair {
		if len(pair) < 2 {
			continue
		}

		vals := pair[:len(pair)-1]
		if len(vals) == 0 {
			continue
		}

		col := pair[len(pair)-1]
		phs := make([]string, 0, len(vals))
		for _, val := range vals {
			phs = append(phs, fmt.Sprintf("$%d", argIndex))
			args = append(args, val)
			argIndex += 1
		}
		conds = append(conds, fmt.Sprintf("%s IN (%s)", col, strings.Join(phs, ",")))
	}

	return conds, args
}

func GetBrc20LatestHeightFromDB() (int, error) {
	row := SwapDB.QueryRow(`SELECT block_height FROM brc20_user_balance ORDER BY block_height DESC LIMIT 1`)
	height := 0
	if err := row.Scan(&height); err != nil {
		if err == sql.ErrNoRows {
			return 0, nil
		}
		return 0, err
	}
	return height, nil
}

func LoadFromDbTickerInfoMap() (map[string]*model.BRC20TokenInfo, error) {
	rows, err := SwapDB.Query(`
SELECT t1.block_height, t1.tick, t1.max_supply, t1.decimals, t1.limit_per_mint, t1.minted, t1.pkscript_deployer, t1.self_mint
FROM brc20_ticker_info t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, tick FROM brc20_ticker_info GROUP BY tick
) t2 ON t1.block_height = t2.block_height AND t1.tick = t2.tick;
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var (
		height   int
		tick     string
		max      string
		decimals uint8
		limit    string
		minted   string
		pkscript string
		selfmint bool
	)
	tickerInfoMap := make(map[string]*model.BRC20TokenInfo)

	for rows.Next() {
		if err := rows.Scan(&height, &tick, &max, &decimals, &limit, &minted, &pkscript, &selfmint); err != nil {
			return nil, err
		}

		nminted := decimal.MustNewDecimalFromString(minted, int(decimals))
		nmax := decimal.MustNewDecimalFromString(max, int(decimals))

		uniqueLowerTicker := strings.ToLower(tick)

		sselfmint := ""
		if selfmint {
			sselfmint = "true"
		}
		tickerInfoMap[uniqueLowerTicker] = &model.BRC20TokenInfo{
			Ticker: tick,
			Deploy: &model.InscriptionBRC20TickInfo{
				SelfMint:    selfmint,
				Max:         nmax,
				Decimal:     decimals,
				Limit:       decimal.MustNewDecimalFromString(limit, int(decimals)),
				TotalMinted: nminted,
				Data: &model.InscriptionBRC20InfoResp{
					Operation:     "deploy",
					BRC20Tick:     tick,
					BRC20Max:      max,
					BRC20Limit:    limit,
					BRC20Amount:   "",
					BRC20Decimal:  fmt.Sprintf("%d", decimals),
					BRC20Minted:   minted,
					BRC20SelfMint: sselfmint,
				},
			},
		}
	}

	return tickerInfoMap, nil
}

func LoadFromDbUserTokensBalanceData(tokenInfos map[string]*model.BRC20TokenInfo, pkscripts, ticks []string) (
	map[string]map[string]*model.BRC20TokenBalance, // [address][ticker]balanc
	error,
) {

	inConds, inCondArgs := buildSQLWhereInStr([][]string{
		append(pkscripts, "pkscript"),
		append(ticks, "tick"),
	})
	condSql := ""
	if len(inConds) > 0 {
		condSql = "WHERE " + strings.Join(inConds, " AND ")
	}

	sql := fmt.Sprintf(`
SELECT t1.tick, t1.pkscript, t1.block_height, t1.available_balance, t1.transferable_balance
FROM brc20_user_balance t1
INNER JOIN (
	SELECT tick, pkscript, MAX(block_height) AS max_block_height
	FROM brc20_user_balance %s GROUP BY tick, pkscript
) t2 ON t1.tick = t2.tick AND t1.pkscript = t2.pkscript AND t1.block_height = t2.max_block_height
`, condSql)
	args := inCondArgs

	rows, err := SwapDB.Query(sql, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var (
		tick         string
		pkscript     string
		height       int
		available    string
		transferable string
		decimals     uint8
	)

	userTokensBalanceMap := make(map[string]map[string]*model.BRC20TokenBalance)
	for rows.Next() {
		if err := rows.Scan(&tick, &pkscript, &height, &available, &transferable); err != nil {
			return nil, err
		}

		lowerTick := strings.ToLower(tick)
		if info, ok := tokenInfos[lowerTick]; !ok {
			return nil, fmt.Errorf("token info not found for ticker: %s", lowerTick)
		} else {
			decimals = info.Deploy.Decimal
		}

		ab := decimal.MustNewDecimalFromString(available, int(decimals))
		tb := decimal.MustNewDecimalFromString(transferable, int(decimals))

		balance := &model.BRC20TokenBalance{
			Ticker:              tick,
			PkScript:            pkscript,
			AvailableBalance:    ab,
			TransferableBalance: tb,
			ValidTransferMap:    make(map[string]*model.InscriptionBRC20TickInfo),
		}

		if _, ok := userTokensBalanceMap[pkscript]; !ok {
			userTokensBalanceMap[pkscript] = make(map[string]*model.BRC20TokenBalance)
		}

		userTokensBalanceMap[pkscript][lowerTick] = balance
	}

	return userTokensBalanceMap, nil
}

func UserTokensBalanceMap2TokenUsersBalanceMap(
	tokenInfos map[string]*model.BRC20TokenInfo,
	userTokensMap map[string]map[string]*model.BRC20TokenBalance) map[string]map[string]*model.BRC20TokenBalance {
	// [ticker][address]balanc
	tokenUsersMap := make(map[string]map[string]*model.BRC20TokenBalance)

	// init all ticks
	for tick := range tokenInfos {
		tokenUsersMap[tick] = make(map[string]*model.BRC20TokenBalance)
	}

	for pkscript, userTokensBalance := range userTokensMap {
		for tick, balance := range userTokensBalance {
			if balance.AvailableBalance.Sign() == 0 && balance.TransferableBalance.Sign() == 0 {
				continue
			}
			tokenUsersMap[tick][pkscript] = balance
		}
	}
	return tokenUsersMap
}

func LoadFromDBTransferStateMap() (res map[string]uint32, err error) {
	rows, err := SwapDB.Query(`
SELECT t1.block_height, t1.create_key FROM brc20_transfer_state  t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key
	FROM brc20_transfer_state
	WHERE moved = true
	GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var (
		height     uint32
		create_key string
	)
	for rows.Next() {
		if err := rows.Scan(&height, &create_key); err != nil {
			return nil, err
		}
		res[create_key] = height
	}

	return res, nil
}

func LoadFromDBValidTransferMap(tokenInfos map[string]*model.BRC20TokenInfo) (res map[string]*model.InscriptionBRC20TickInfo, err error) {
	query := `
SELECT t1.block_height, t1.create_key, t1.tick, t1.pkscript, t1.amount,
	   t1.inscription_number, t1.inscription_id,
	   t1.txid, t1.vout, t1.output_value, t1.output_offset
FROM brc20_valid_transfer t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key FROM brc20_valid_transfer GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
LEFT JOIN brc20_transfer_state t3 ON t1.create_key = t3.create_key
WHERE t3.moved IS NULL
`
	// log.Println("query", query)

	rows, err := SwapDB.Query(query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	res = make(map[string]*model.InscriptionBRC20TickInfo)
	for rows.Next() {
		var inscId string
		t := model.InscriptionBRC20TickInfo{}

		var (
			amount   string
			decimals uint8
		)

		if err := rows.Scan(&t.Height, &t.CreateIdxKey, &t.Tick, &t.PkScript, &amount,
			&t.InscriptionNumber, &inscId,
			&t.TxId, &t.Vout, &t.Satoshi, &t.Offset,
		); err != nil {
			return nil, err
		}

		lowerTick := strings.ToLower(t.Tick)
		if info, ok := tokenInfos[lowerTick]; !ok {
			return nil, fmt.Errorf("token info not found for ticker: %s", lowerTick)
		} else {
			decimals = info.Deploy.Decimal
		}
		t.Amount = decimal.MustNewDecimalFromString(amount, int(decimals))

		t.Meta = &model.InscriptionBRC20Data{
			IsTransfer:        false,
			TxId:              t.TxId,
			Idx:               0,
			Vout:              t.Vout,
			Offset:            t.Offset,
			Satoshi:           t.Satoshi,
			PkScript:          t.PkScript,
			Fee:               0,
			InscriptionNumber: t.InscriptionNumber,
			ContentBody:       []byte{},
			CreateIdxKey:      t.CreateIdxKey,
			Height:            t.Height,
			TxIdx:             0,
			BlockTime:         0,
			Sequence:          0,
			InscriptionId:     inscId,
		}
		res[t.CreateIdxKey] = &t
	}
	return res, nil
}

func LoadFromDBModuleInfoMap() (map[string]*model.BRC20ModuleSwapInfo, error) {
	rows, err := SwapDB.Query(`
SELECT module_id, name, pkscript_deployer, pkscript_sequencer, pkscript_gas_to, pkscript_lp_fee, gas_tick, fee_rate_swap
FROM brc20_swap_info
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	modulesInfoMap := make(map[string]*model.BRC20ModuleSwapInfo)
	for rows.Next() {
		info := model.BRC20ModuleSwapInfo{
			History:                               make([]*model.BRC20ModuleHistory, 0),
			CommitInvalidMap:                      make(map[string]struct{}, 0),
			CommitIdChainMap:                      make(map[string]struct{}, 0),
			CommitIdMap:                           make(map[string]struct{}, 0),
			UsersTokenBalanceDataMap:              make(map[string]map[string]*model.BRC20ModuleTokenBalance, 0),
			TokenUsersBalanceDataMap:              make(map[string]map[string]*model.BRC20ModuleTokenBalance, 0),
			LPTokenUsersBalanceMap:                make(map[string]map[string]*decimal.Decimal, 0),
			LPTokenUsersBalanceUpdatedMap:         make(map[string]struct{}, 0),
			UsersLPTokenBalanceMap:                make(map[string]map[string]*decimal.Decimal, 0),
			SwapPoolTotalBalanceDataMap:           make(map[string]*model.BRC20ModulePoolTotalBalance, 0),
			ConditionalApproveStateBalanceDataMap: make(map[string]*model.BRC20ModuleConditionalApproveStateBalance, 0),
		}
		err := rows.Scan(
			&info.ID,
			&info.Name,
			&info.DeployerPkScript,
			&info.SequencerPkScript,
			&info.GasToPkScript,
			&info.LpFeePkScript,
			&info.GasTick,
			&info.FeeRateSwap)
		if err != nil {
			return nil, err
		}
		modulesInfoMap[info.ID] = &info
	}
	return modulesInfoMap, nil
}

func LoadFromDBModuleHistoryMap(moduleId string) (map[string][]*model.BRC20ModuleHistory, error) {
	query := `
SELECT module_id, history_type, valid, txid, idx, vout,
	output_value, output_offset, pkscript_from, pkscript_to, fee, txidx, block_time,
	inscription_number, inscription_id, inscription_content
FROM brc20_swap_history WHERE module_id = $1
`
	rows, err := SwapDB.Query(query, moduleId)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	historyMap := make(map[string][]*model.BRC20ModuleHistory)
	for rows.Next() {
		var h model.BRC20ModuleHistory
		var moduleId string
		err := rows.Scan(&moduleId, &h.Type, &h.Valid, &h.TxId, &h.Idx, &h.Vout,
			&h.Satoshi, &h.Offset, &h.PkScriptFrom, &h.PkScriptTo, &h.Fee, &h.TxIdx, &h.BlockTime,
			&h.Inscription.InscriptionNumber,
			&h.Inscription.InscriptionId,
			&h.Inscription.ContentBody)
		if err != nil {
			return nil, err
		}
		historyMap[moduleId] = append(historyMap[moduleId], &h)
	}
	return historyMap, nil
}

type DBModelSwapCommitChain struct {
	ModuleID  string
	CommitID  string
	Valid     bool
	Connected bool
}

func LoadModuleCommitChain(moduleId string, commitIds []string) ([]*DBModelSwapCommitChain, error) {
	inConds, inCondArgs := buildSQLWhereInStr([][]string{append(commitIds, "commit_id")})
	condSql := "WHERE "
	if len(inConds) > 0 {
		condSql = strings.Join(inConds, " AND ")
	}
	condSql += fmt.Sprintf("module_id = $%d", len(inConds)+1)

	query := fmt.Sprintf(`
SELECT module_id, commit_id, valid, connected
FROM brc20_swap_commit_chain %s
`, condSql)
	args := append(inCondArgs, moduleId)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("querying commit chain failed: %w", err)
	}
	defer rows.Close()

	var commitChains []*DBModelSwapCommitChain
	for rows.Next() {
		var cc DBModelSwapCommitChain
		if err := rows.Scan(&cc.ModuleID, &cc.CommitID, &cc.Valid, &cc.Connected); err != nil {
			return nil, fmt.Errorf("scanning commit chain row failed: %w", err)
		}
		commitChains = append(commitChains, &cc)
	}

	return commitChains, nil
}

func LoadFromDBModuleUserBalanceMap(moduleId string, ticks []string, pkscripts []string) (
	map[string]map[string]*model.BRC20ModuleTokenBalance, // [tick][address]balanceData
	error,
) {
	conds, args := buildSQLWhereInStr([][]string{
		append(pkscripts, "pkscript"),
		append(ticks, "tick"),
		append([]string{moduleId}, "module_id"),
	})

	query := fmt.Sprintf(`
SELECT t1.module_id, t1.tick, t1.pkscript, t1.swap_balance, t1.available_balance,
	t1.approveable_balance, t1.cond_approveable_balance, t1.ready_to_withdraw_amount
FROM brc20_swap_user_balance t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, tick, pkscript
	FROM brc20_swap_user_balance
	WHERE %s
	GROUP BY tick, pkscript
) t2 ON t1.block_height = t2.block_height AND t1.tick = t2.tick AND t1.pkscript = t2.pkscript
`, strings.Join(conds, " AND "))
	// log.Println("query", query)
	// log.Println("args", args)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]map[string]*model.BRC20ModuleTokenBalance)
	for rows.Next() {
		var moduleId, tick, pkscript string
		var balance model.BRC20ModuleTokenBalance
		err := rows.Scan(&moduleId, &tick, &pkscript, &balance.SwapAccountBalance, &balance.AvailableBalance, &balance.ApproveableBalance, &balance.CondApproveableBalance, &balance.ReadyToWithdrawAmount)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}

		if _, ok := result[pkscript]; !ok {
			result[pkscript] = make(map[string]*model.BRC20ModuleTokenBalance)
		}
		balance.Tick = tick
		balance.PkScript = pkscript

		lowerTiker := strings.ToLower(tick)
		result[pkscript][lowerTiker] = &balance
	}

	return result, nil
}

func LoadFromDBModulePoolLpBalanceMap(moduleId string, pools []string) (
	map[string]*model.BRC20ModulePoolTotalBalance, error) {

	inConds, inArgs := buildSQLWhereInStr([][]string{
		append(pools, "pool"),
	}, 2)
	tickInCondSql := ""
	if len(inConds) > 0 {
		tickInCondSql = "AND " + strings.Join(inConds, " AND ")
	}

	query := fmt.Sprintf(`
SELECT t1.module_id, pool, t1.tick0, t1.tick0_balance, t1.tick1, t1.tick1_balance, t1.lp_balance
FROM brc20_swap_pool_balance t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, module_id, tick0, tick1
	FROM brc20_swap_pool_balance
	WHERE module_id = $1 %s
	GROUP BY module_id, tick0, tick1
) t2 ON t1.block_height = t2.block_height AND t1.module_id = t2.module_id
	AND t1.tick0 = t2.tick0 AND t1.tick1 = t2.tick1
`, tickInCondSql)
	args := append([]any{moduleId}, inArgs...)

	// log.Println("query:", query)
	// log.Println("args:", args)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]*model.BRC20ModulePoolTotalBalance)
	for rows.Next() {
		var module_id, pool string
		var balance model.BRC20ModulePoolTotalBalance
		err := rows.Scan(&module_id, &pool,
			&balance.Tick[0], &balance.TickBalance[0],
			&balance.Tick[1], &balance.TickBalance[1],
			&balance.LpBalance)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[pool] = &balance
	}

	return result, nil
}

func LoadFromDBModuleUserLpBalanceMap(moduleId string, pools []string, pkscripts []string) (
	map[string]map[string]*decimal.Decimal, error) {
	conds, args := buildSQLWhereInStr([][]string{
		append(pkscripts, "pkscript"),
		append(pools, "pool"),
		append([]string{moduleId}, "module_id"),
	})

	query := fmt.Sprintf(`
SELECT t1.module_id, t1.pool, t1.pkscript, t1.lp_balance
FROM brc20_swap_user_lp_balance t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, module_id, pool, pkscript
	FROM brc20_swap_user_lp_balance
	WHERE %s
	GROUP BY module_id, pool, pkscript
) t2 ON t1.block_height = t2.block_height AND t1.module_id = t2.module_id AND t1.pool = t2.pool AND t1.pkscript = t2.pkscript
`, strings.Join(conds, " AND "))

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]map[string]*decimal.Decimal)
	for rows.Next() {
		var moduleId, pool, pkscript string
		var lpBalance decimal.Decimal
		err := rows.Scan(&moduleId, &pool, &pkscript, &lpBalance)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}

		if _, ok := result[pkscript]; !ok {
			result[pkscript] = make(map[string]*decimal.Decimal)
		}
		result[pkscript][pool] = &lpBalance
	}

	return result, nil
}

func LoadFromDBSwapApproveStateMap(createKeys []string) (map[string]uint32, error) {
	inConds, inArgs := buildSQLWhereInStr([][]string{append(createKeys, "create_key")})
	condSql := ""
	if len(inConds) > 0 {
		condSql = "WHERE " + strings.Join(inConds, " AND") + " AND moved = true"
	} else {
		condSql = "WHERE moved = true"
	}

	query := fmt.Sprintf(`
SELECT t1.block_height, t1.create_key
FROM brc20_swap_approve_state t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key
	FROM brc20_swap_approve_state %s
	GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
`, condSql)

	rows, err := SwapDB.Query(query, inArgs...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]uint32)
	for rows.Next() {
		var createKey string
		var height uint32
		err := rows.Scan(&height, &createKey)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = height
	}

	return result, nil
}

func LoadFromDBSwapApproveMap(createKeys []string) (map[string]*model.InscriptionBRC20SwapInfo, error) {
	inConds, inArgs := buildSQLWhereInStr([][]string{append(createKeys, "create_key")})
	condSql := ""
	if len(inConds) > 0 {
		condSql = "WHERE " + strings.Join(inConds, " AND")
	}

	query := fmt.Sprintf(`
SELECT t1.block_height, t1.create_key, t1.module_id, t1.tick, t1.pkscript, t1.amount,
	t1.inscription_number, t1.inscription_id,
	t1.txid, t1.vout, t1.output_value, t1.output_offset
FROM brc20_swap_valid_approve t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key
	FROM brc20_swap_valid_approve %s
	GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
LEFT JOIN brc20_swap_approve_state t3 ON t3.create_key = t1.create_key
WHERE t3.moved IS NULL
`, condSql)

	// 执行查询
	rows, err := SwapDB.Query(query, inArgs...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]*model.InscriptionBRC20SwapInfo)
	for rows.Next() {
		info := model.InscriptionBRC20SwapInfo{
			Data: &model.InscriptionBRC20Data{},
		}
		var createKey string
		var height int
		err := rows.Scan(
			&height, &createKey, &info.Module, &info.Tick, &info.Data.PkScript, &info.Amount,
			&info.Data.InscriptionNumber, &info.Data.InscriptionId,
			&info.Data.TxId, &info.Data.Vout, &info.Data.Satoshi, &info.Data.Offset,
		)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = &info
	}

	return result, nil
}

func LoadFromDBSwapCondApproveStateMap(createKeys []string) (map[string]uint32, error) {
	inConds, inArgs := buildSQLWhereInStr([][]string{append(createKeys, "create_key")})
	inCondSql := ""
	if len(inConds) > 0 {
		inCondSql = "AND " + strings.Join(inConds, " AND ")
	}

	query := fmt.Sprintf(`
SELECT t1.create_key, t1.moved
FROM brc20_swap_cond_approve_state t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key
	FROM brc20_swap_cond_approve_state
	WHERE moved = true %s
	GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
`, inCondSql)

	rows, err := SwapDB.Query(query, inArgs...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]uint32)
	for rows.Next() {
		var createKey string
		var height uint32 // fixme
		var moved bool
		err := rows.Scan(&createKey, &moved)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = height
	}

	return result, nil
}

func LoadFromDBSwapCondApproveMap(createKeys []string) (map[string]*model.InscriptionBRC20SwapConditionalApproveInfo, error) {
	inConds, inArgs := buildSQLWhereInStr([][]string{append(createKeys, "create_key")})
	inCondSql := ""
	if len(inConds) > 0 {
		inCondSql = "WHERE " + strings.Join(inConds, " AND ")
	}

	query := fmt.Sprintf(`
SELECT t1.block_height, t1.create_key, t1.module_id, t1.tick, t1.pkscript, t1.amount,
	t1.inscription_number, t1.inscription_id,
	t1.txid, t1.vout, t1.output_value, t1.output_offset
FROM brc20_swap_valid_cond_approve t1
INNER JOIN (
	SELECT MAX(block_height) as block_height, create_key
	FROM brc20_swap_valid_cond_approve %s
	GROUP BY create_key
) t2 ON t1.block_height = t2.block_height AND t1.create_key = t2.create_key
LEFT JOIN brc20_swap_cond_approve_state t3 ON t1.create_key = t3.create_key
WHERE t3.moved IS NULL
`, inCondSql)

	rows, err := SwapDB.Query(query, inArgs...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]*model.InscriptionBRC20SwapConditionalApproveInfo)
	for rows.Next() {
		var height int
		var createKey string
		info := model.InscriptionBRC20SwapConditionalApproveInfo{
			Data: &model.InscriptionBRC20Data{},
		}
		err := rows.Scan(
			&height, &createKey, &info.Module, &info.Tick, &info.Data.PkScript, &info.Amount,
			&info.Data.InscriptionNumber, &info.Data.InscriptionId,
			&info.Data.TxId, &info.Data.Vout, &info.Data.Satoshi, &info.Data.Offset,
		)
		if err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = &info
	}

	return result, nil
}

func LoadFromDBSwapCommitStateMap(createKeys []string) (map[string]uint32, error) {
	whereCond := "WHERE moved = true"
	args := []any{}
	if createKeys == nil {
		whereCond = "WHERE create_key = ANY($1) AND moved = true"
		args = append(args, pq.Array(createKeys))
	}

	query := fmt.Sprintf(`
SELECT cs.create_key, cs.moved
FROM brc20_swap_commit_state cs
INNER JOIN (
	SELECT create_key, MAX(block_height) AS max_height
	FROM brc20_swap_commit_state %s GROUP BY create_key
) sub ON cs.create_key = sub.create_key AND cs.block_height = sub.max_height
`, whereCond)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]uint32)
	for rows.Next() {
		var createKey string
		var height uint32 // fixme: not set
		var moved bool
		if err := rows.Scan(&createKey, &moved); err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = height
	}

	return result, nil
}

func LoadFromDBSwapCommitMap(createKeys []string) (map[string]*model.InscriptionBRC20Data, error) {
	whereCond := ""
	args := []any{}
	if len(createKeys) != 0 {
		whereCond = "WHERE create_key = ANY($1)"
		args = append(args, pq.Array(createKeys))
	}

	query := fmt.Sprintf(`
SELECT vc.block_height, vc.module_id, vc.create_key, vc.pkscript,
	vc.inscription_number, vc.inscription_id,
    vc.txid, vc.vout, vc.output_value, vc.output_offset, vc.inscription_content
FROM brc20_swap_valid_commit vc
INNER JOIN (
	SELECT create_key, MAX(block_height) AS max_height
	FROM brc20_swap_valid_commit %s GROUP BY create_key
) sub ON vc.create_key = sub.create_key AND vc.block_height = sub.max_height
LEFT JOIN brc20_swap_commit_state cs ON vc.create_key = cs.create_key
WHERE cs.moved IS NULL
`, whereCond)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]*model.InscriptionBRC20Data)
	for rows.Next() {
		var info model.InscriptionBRC20Data
		var height int
		var moduleId, createKey string
		if err := rows.Scan(&height, &moduleId, &createKey, &info.PkScript,
			&info.InscriptionNumber, &info.InscriptionId,
			&info.TxId, &info.Vout, &info.Satoshi, &info.Offset, &info.ContentBody); err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = &info
	}
	return result, nil
}

func LoadFromDBSwapWithdrawStateMap(createKeys []string) (map[string]uint32, error) {
	whereCond := ""
	args := []any{}
	if len(createKeys) > 0 {
		whereCond = "WHERE create_key = ANY($1)"
		args = append(args, pq.Array(createKeys))
	}

	query := fmt.Sprintf(`
SELECT ws.create_key, ws.moved
FROM brc20_swap_withdraw_state ws
INNER JOIN (
	SELECT create_key, MAX(block_height) AS max_height
	FROM brc20_swap_withdraw_state %s GROUP BY create_key
) sub ON ws.create_key = sub.create_key AND ws.block_height = sub.max_height
`, whereCond)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]uint32)
	for rows.Next() {
		var createKey string
		var height uint32 // fixme
		var moved bool
		if err := rows.Scan(&createKey, &moved); err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = height
	}

	return result, nil
}

func LoadFromDBSwapWithdrawMap(createKeys []string) (map[string]*model.InscriptionBRC20SwapInfo, error) {
	whereCond := ""
	args := []any{}
	if len(createKeys) > 0 {
		whereCond = "WHERE create_key = ANY($1)"
		args = append(args, pq.Array(createKeys))
	}

	query := fmt.Sprintf(`
SELECT vw.block_height, vw.create_key, vw.module_id,
	vw.tick, vw.pkscript, vw.amount,
    vw.inscription_number, vw.inscription_id,
	vw.txid, vw.vout, vw.output_value, vw.output_offset
FROM brc20_swap_valid_withdraw vw
INNER JOIN (
	SELECT create_key, MAX(block_height) AS max_height
	FROM brc20_swap_valid_withdraw %s GROUP BY create_key
) sub ON vw.create_key = sub.create_key AND vw.block_height = sub.max_height
LEFT JOIN brc20_swap_withdraw_state ws ON vw.create_key = ws.create_key
WHERE ws.moved IS NULL
`, whereCond)

	rows, err := SwapDB.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query failed: %w", err)
	}
	defer rows.Close()

	result := make(map[string]*model.InscriptionBRC20SwapInfo)
	for rows.Next() {
		var (
			info      model.InscriptionBRC20SwapInfo
			height    int
			createKey string
		)
		if err := rows.Scan(&height, &createKey, &info.Module,
			&info.Tick, &info.Data.PkScript, &info.Amount,
			&info.Data.InscriptionNumber, &info.Data.InscriptionId,
			&info.Data.TxId, &info.Data.Vout, &info.Data.Satoshi, &info.Data.Offset); err != nil {
			return nil, fmt.Errorf("scan failed: %w", err)
		}
		result[createKey] = &info
	}
	return result, nil
}
