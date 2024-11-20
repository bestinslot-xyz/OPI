package loader

import (
	"database/sql"
	"fmt"
	"log"

	_ "github.com/lib/pq"
	"github.com/unisat-wallet/libbrc20-indexer/model"
)

var (
	SwapDB  *sql.DB
	MetaDB  *sql.DB
	BRC20DB *sql.DB
)

func Init(psqlInfo string) {
	var err error
	SwapDB, err = sql.Open("postgres", psqlInfo)
	if err != nil {
		log.Panic("Connect PG Failed: ", err)
	}
	if err := SwapDB.Ping(); err != nil {
		log.Panic("Ping SwapDB Failed: ", err)
	}

	SwapDB.SetMaxOpenConns(2000)
	SwapDB.SetMaxIdleConns(1000)
}

func InitMetaDB(psqlInfo string) {
	var err error
	MetaDB, err = sql.Open("postgres", psqlInfo)
	if err != nil {
		log.Panic("Connect PG Failed: ", err)
	}
	if err := MetaDB.Ping(); err != nil {
		log.Panic("Ping MetaDB Failed: ", err)
	}

	MetaDB.SetMaxOpenConns(2000)
	MetaDB.SetMaxIdleConns(1000)
}

func InitBRC20DB(psqlInfo string) {
	var err error
	BRC20DB, err = sql.Open("postgres", psqlInfo)
	if err != nil {
		log.Panic("Connect PG Failed: ", err)
	}
	if err := BRC20DB.Ping(); err != nil {
		log.Panic("Ping BRC20DB Failed: ", err)
	}

	BRC20DB.SetMaxOpenConns(2000)
	BRC20DB.SetMaxIdleConns(1000)
}

func MustBegin() *sql.Tx {
	tx, err := SwapDB.Begin()
	if err != nil {
		log.Panic("PG Begin Wrong: ", err)
	}
	return tx
}

// brc20_ticker_info
func SaveDataToDBTickerInfoMap(tx *sql.Tx, height uint32,
	inscriptionsTickerInfoMap map[string]*model.BRC20TokenInfo,
) {
	stmtTickerInfo, err := tx.Prepare(`
INSERT INTO brc20_ticker_info(block_height, tick, max_supply, decimals, limit_per_mint, minted, pkscript_deployer, self_mint)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
`)

	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for _, info := range inscriptionsTickerInfoMap {
		if info.UpdateHeight != height {
			continue
		}
		// save ticker info
		res, err := stmtTickerInfo.Exec(height, info.Ticker,
			info.Deploy.Data.BRC20Max,
			info.Deploy.Data.BRC20Decimal,
			info.Deploy.Data.BRC20Limit,
			info.Deploy.TotalMinted.String(),
			info.Deploy.PkScript,
			info.Deploy.SelfMint,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBTickerBalanceMap(tx *sql.Tx, height uint32,
	userTokensBalanceData map[string]map[string]*model.BRC20TokenBalance,
) {
	stmtUserBalance, err := tx.Prepare(`
INSERT INTO brc20_user_balance(block_height, tick, pkscript, available_balance, transferable_balance)
VALUES ($1, $2, $3, $4, $5)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for _, tokensMap := range userTokensBalanceData {
		// holders
		for _, balanceData := range tokensMap {
			if balanceData.UpdateHeight != height {
				continue
			}

			// save balance db
			res, err := stmtUserBalance.Exec(height,
				balanceData.Ticker,
				balanceData.PkScript,
				balanceData.AvailableBalance.String(),
				balanceData.TransferableBalance.String(),
			)
			if err != nil {
				log.Panic("PG Statements Exec Wrong: ", err)
			}

			if _, err := res.RowsAffected(); err != nil {
				log.Panic("PG Affecte Wrong: ", err)
			}
		}
	}
}

func SaveDataToDBTickerHistoryMap(tx *sql.Tx, height uint32,
	allHistory []*model.BRC20History,
) {
	stmtBRC20History, err := tx.Prepare(`
INSERT INTO brc20_history(block_height, tick,
	history_type,
	valid,
	txid,
	idx,
	vout,
	output_value,
	output_offset,
	pkscript_from,
	pkscript_to,
	fee,
	txidx,
	block_time,
	inscription_number,
	inscription_id,
	inscription_content,
	amount,
	available_balance,
	transferable_balance) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for _, h := range allHistory {
		if h.Height != height {
			continue
		}

		if !h.Valid {
			continue
		}

		{
			res, err := stmtBRC20History.Exec(height, h.Inscription.Data.BRC20Tick,
				h.Type, h.Valid,
				h.TxId, h.Idx, h.Vout, h.Satoshi, h.Offset,
				h.PkScriptFrom, h.PkScriptTo,
				h.Fee,
				h.TxIdx, h.BlockTime,
				h.Inscription.InscriptionNumber, h.Inscription.InscriptionId,
				[]byte("{}"), // content
				h.Amount, h.AvailableBalance, h.TransferableBalance,
			)
			if err != nil {
				log.Panic("PG Statements Exec Wrong: ", err)
			}
			if _, err := res.RowsAffected(); err != nil {
				log.Panic("PG Affecte Wrong: ", err)
			}

		}
	}
}

func SaveDataToDBTransferStateMap(tx *sql.Tx, height uint32,
	inscriptionsTransferRemoveMap map[string]uint32,
) {
	stmtTransferState, err := tx.Prepare(`
INSERT INTO brc20_transfer_state(block_height, create_key, moved)
VALUES ($1, $2, $3)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for createKey, removeHeight := range inscriptionsTransferRemoveMap {
		if removeHeight != height {
			continue
		}

		res, err := stmtTransferState.Exec(height, createKey, true)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBValidTransferMap(tx *sql.Tx, height uint32,
	inscriptionsValidTransferMap map[string]*model.InscriptionBRC20TickInfo,
) {
	stmtValidTransfer, err := tx.Prepare(`
INSERT INTO brc20_valid_transfer(block_height, create_key, tick, pkscript, amount,
	inscription_number, inscription_id, txid, vout, output_value, output_offset)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for create_key, transferInfo := range inscriptionsValidTransferMap {
		if transferInfo.Height != height {
			continue
		}

		res, err := stmtValidTransfer.Exec(height,
			create_key,
			transferInfo.Tick,
			transferInfo.PkScript,
			transferInfo.Amount.String(),
			transferInfo.InscriptionNumber, transferInfo.Meta.GetInscriptionId(),
			transferInfo.TxId, transferInfo.Vout, transferInfo.Satoshi, transferInfo.Offset,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}

}

func SaveDataToDBModuleInfoMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {

	stmtSwapInfo, err := tx.Prepare(`
INSERT INTO brc20_swap_info(block_height, module_id,
	name,
	pkscript_deployer,
	pkscript_sequencer,
	pkscript_gas_to,
	pkscript_lp_fee,
	gas_tick,
	fee_rate_swap
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for moduleId, info := range modulesInfoMap {
		if info.UpdateHeight != height {
			continue
		}

		// save swap info db
		res, err := stmtSwapInfo.Exec(height, moduleId,
			info.Name,
			info.DeployerPkScript,
			info.SequencerPkScript,
			info.GasToPkScript,
			info.LpFeePkScript,
			info.GasTick,
			info.FeeRateSwap,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBModuleHistoryMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {

	stmtSwapHistory, err := tx.Prepare(`
INSERT INTO brc20_swap_history(block_height, module_id,
	history_type,
	valid,
	txid,
	idx,
	vout,
	output_value,
	output_offset,
	pkscript_from,
	pkscript_to,
	fee,
	txidx,
	block_time,
	inscription_number,
	inscription_id,
	inscription_content
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for moduleId, info := range modulesInfoMap {

		nValid := 0
		// history
		for _, h := range info.History {
			if h.Height != height {
				continue
			}

			if h.Valid {
				nValid++
			}
			if !h.Valid {
				continue
			}

			{
				res, err := stmtSwapHistory.Exec(height, moduleId,
					h.Type, h.Valid,
					h.TxId, h.Idx, h.Vout, h.Satoshi, h.Offset,
					h.PkScriptFrom, h.PkScriptTo,
					h.Fee,
					h.TxIdx, h.BlockTime,
					h.Inscription.InscriptionNumber, h.Inscription.InscriptionId,
					[]byte("{}"), // content
				)
				if err != nil {
					log.Panic("PG Statements Exec Wrong: ", err)
				}
				if _, err := res.RowsAffected(); err != nil {
					log.Panic("PG Affecte Wrong: ", err)
				}

			}

		}

	}
}

// approve
func SaveDataToDBSwapApproveStateMap(tx *sql.Tx, height uint32,
	inscriptionsApproveRemoveMap map[string]uint32,
) {
	stmtApproveState, err := tx.Prepare(`
INSERT INTO brc20_swap_approve_state(block_height, create_key, moved)
VALUES ($1, $2, $3)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, removeHeight := range inscriptionsApproveRemoveMap {
		if removeHeight != height {
			continue
		}

		res, err := stmtApproveState.Exec(height, createKey, true)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBSwapApproveMap(tx *sql.Tx, height uint32,
	inscriptionsValidApproveMap map[string]*model.InscriptionBRC20SwapInfo,
) {
	stmtValidApprove, err := tx.Prepare(`
INSERT INTO brc20_swap_valid_approve(block_height, module_id, create_key, tick, pkscript, amount,
	inscription_number, inscription_id, txid, vout, output_value, output_offset)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, approveInfo := range inscriptionsValidApproveMap {
		if approveInfo.Data.Height != height {
			continue
		}

		res, err := stmtValidApprove.Exec(height,
			approveInfo.Module,
			createKey,
			approveInfo.Tick,
			approveInfo.Data.PkScript,
			approveInfo.Amount.String(),
			approveInfo.Data.InscriptionNumber, approveInfo.Data.GetInscriptionId(),
			approveInfo.Data.TxId, approveInfo.Data.Vout, approveInfo.Data.Satoshi, approveInfo.Data.Offset,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

// cond approve
func SaveDataToDBSwapCondApproveStateMap(tx *sql.Tx, height uint32,
	inscriptionsCondApproveRemoveMap map[string]uint32,
) {
	stmtCondApproveState, err := tx.Prepare(`
INSERT INTO brc20_swap_cond_approve_state(block_height, create_key, moved)
VALUES ($1, $2, $3)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, removeHeight := range inscriptionsCondApproveRemoveMap {
		if removeHeight != height {
			continue
		}

		res, err := stmtCondApproveState.Exec(height, createKey, true)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBSwapCondApproveMap(tx *sql.Tx, height uint32,
	inscriptionsValidConditionalApproveMap map[string]*model.InscriptionBRC20SwapConditionalApproveInfo,
) {
	stmtValidCondApprove, err := tx.Prepare(`
INSERT INTO brc20_swap_valid_cond_approve(block_height, create_key, module_id, tick, pkscript, amount,
	inscription_number, inscription_id, txid, vout, output_value, output_offset)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, condApproveInfo := range inscriptionsValidConditionalApproveMap {
		if condApproveInfo.UpdateHeight != height {
			continue
		}

		res, err := stmtValidCondApprove.Exec(height,
			createKey,
			condApproveInfo.Module,
			condApproveInfo.Tick,
			condApproveInfo.Data.PkScript,
			condApproveInfo.Amount.String(),
			condApproveInfo.Data.InscriptionNumber, condApproveInfo.Data.GetInscriptionId(),
			condApproveInfo.Data.TxId, condApproveInfo.Data.Vout, condApproveInfo.Data.Satoshi, condApproveInfo.Data.Offset,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

// withdraw
func SaveDataToDBSwapWithdrawStateMap(tx *sql.Tx, height uint32,
	inscriptionsWithdrawRemoveMap map[string]uint32,
) {
	stmtWithdrawState, err := tx.Prepare(`
INSERT INTO brc20_swap_withdraw_state(block_height, create_key, moved)
VALUES ($1, $2, $3)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for createKey, removeHeight := range inscriptionsWithdrawRemoveMap {
		if removeHeight != height {
			continue
		}

		res, err := stmtWithdrawState.Exec(height, createKey, true)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBSwapWithdrawMap(tx *sql.Tx, height uint32,
	inscriptionsValidWithdrawMap map[string]*model.InscriptionBRC20SwapInfo,
) {
	stmtValidWithdraw, err := tx.Prepare(`
INSERT INTO brc20_swap_valid_withdraw(block_height, create_key, module_id, tick, pkscript, amount,
	inscription_number, inscription_id, txid, vout, output_value, output_offset)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
`)
	if err != nil {
		log.Panic("SaveDataToDBSwapWithdrawMap, PG Statements Wrong: ", err)
	}

	for createKey, withdrawInfo := range inscriptionsValidWithdrawMap {
		if withdrawInfo.Data.Height != height {
			continue
		}

		res, err := stmtValidWithdraw.Exec(height,
			createKey,
			withdrawInfo.Module,
			withdrawInfo.Tick,
			withdrawInfo.Data.PkScript,
			withdrawInfo.Amount.String(),
			withdrawInfo.Data.InscriptionNumber, withdrawInfo.Data.GetInscriptionId(),
			withdrawInfo.Data.TxId, withdrawInfo.Data.Vout, withdrawInfo.Data.Satoshi, withdrawInfo.Data.Offset,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}

	}
}

// commit
func SaveDataToDBSwapCommitStateMap(tx *sql.Tx, height uint32,
	inscriptionsCommitRemoveMap map[string]uint32,
) {
	stmtCommitState, err := tx.Prepare(`
INSERT INTO brc20_swap_commit_state(block_height, create_key, moved)
VALUES ($1, $2, $3)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, removeHeight := range inscriptionsCommitRemoveMap {
		if removeHeight != height {
			continue
		}

		res, err := stmtCommitState.Exec(height, createKey, true)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

func SaveDataToDBSwapCommitMap(tx *sql.Tx, height uint32,
	inscriptionsValidCommitMap map[string]*model.InscriptionBRC20Data,
) {
	stmtValidCommit, err := tx.Prepare(`
INSERT INTO brc20_swap_valid_commit(block_height, module_id, create_key, pkscript,
	inscription_number, inscription_id, txid, vout, output_value, output_offset, inscription_content)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for createKey, commitInfo := range inscriptionsValidCommitMap {
		if commitInfo.Height != height {
			continue
		}

		res, err := stmtValidCommit.Exec(height, "commitInfo.Module", createKey, commitInfo.PkScript,
			commitInfo.InscriptionNumber, commitInfo.GetInscriptionId(),
			commitInfo.TxId, commitInfo.Vout, commitInfo.Satoshi, commitInfo.Offset, commitInfo.ContentBody,
		)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}

// fixme: save by height
func SaveDataToDBModuleCommitChainMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {
	stmtSwapCommitChain, err := tx.Prepare(`
INSERT INTO brc20_swap_commit_chain(block_height, module_id, commit_id, valid, connected)
VALUES ($1, $2, $3, $4, $5)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for moduleId, info := range modulesInfoMap {
		// commit state
		commitState := make(map[string]*[2]bool)
		for commitId := range info.CommitInvalidMap {
			if state, ok := commitState[commitId]; !ok {
				commitState[commitId] = &[2]bool{false, false}
			} else {
				state[0] = false
			}
		}

		for commitId := range info.CommitIdMap {
			if state, ok := commitState[commitId]; !ok {
				commitState[commitId] = &[2]bool{true, false}
			} else {
				state[0] = true
			}
		}
		for commitId := range info.CommitIdChainMap {
			if state, ok := commitState[commitId]; !ok {
				commitState[commitId] = &[2]bool{true, true}
			} else {
				state[1] = true
			}
		}

		// save commit state db
		for commitId, state := range commitState {
			res, err := stmtSwapCommitChain.Exec(height, moduleId,
				commitId,
				state[0], // valid
				state[1], // connected
			)
			if err != nil {
				log.Panic("PG Statements Exec Wrong: ", err)
			}
			if _, err := res.RowsAffected(); err != nil {
				log.Panic("PG Affecte Wrong: ", err)
			}

		}
	}
}

func SaveDataToDBModuleUserBalanceMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {

	stmtUserBalance, err := tx.Prepare(`
INSERT INTO brc20_swap_user_balance(block_height, module_id, tick,
	pkscript, swap_balance, available_balance, approveable_balance, cond_approveable_balance, ready_to_withdraw_amount)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}

	for moduleId, info := range modulesInfoMap {
		for _, tokensMap := range info.UsersTokenBalanceDataMap {
			for _, balanceData := range tokensMap {
				if balanceData.UpdateHeight != height {
					continue
				}

				// save balance db
				res, err := stmtUserBalance.Exec(height, moduleId,
					balanceData.Tick,
					balanceData.PkScript,
					balanceData.SwapAccountBalance.String(),
					balanceData.AvailableBalance.String(),
					balanceData.ApproveableBalance.String(),
					balanceData.CondApproveableBalance.String(),
					balanceData.ReadyToWithdrawAmount.String(),
				)
				if err != nil {
					log.Panic("PG Statements Exec Wrong: ", err)
				}
				if _, err := res.RowsAffected(); err != nil {
					log.Panic("PG Affecte Wrong: ", err)
				}
			}
		}
	}
}

func SaveDataToDBModulePoolLpBalanceMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {

	stmtPoolBalance, err := tx.Prepare(`
INSERT INTO brc20_swap_pool_balance(block_height, module_id, pool, tick0, tick0_balance, tick1, tick1_balance, lp_balance)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for moduleId, info := range modulesInfoMap {
		for pool, swap := range info.SwapPoolTotalBalanceDataMap {
			if swap.UpdateHeight != height {
				continue
			}

			// save swap balance db
			res, err := stmtPoolBalance.Exec(
				height,
				moduleId,
				pool,
				swap.Tick[0],
				swap.TickBalance[0],
				swap.Tick[1],
				swap.TickBalance[1],
				swap.LpBalance.String(),
			)
			if err != nil {
				log.Panic("PG Statements Exec Wrong: ", err)
			}
			if _, err := res.RowsAffected(); err != nil {
				log.Panic("PG Affecte Wrong: ", err)
			}

		}
	}
}

func SaveDataToDBModuleUserLpBalanceMap(tx *sql.Tx, height uint32,
	modulesInfoMap map[string]*model.BRC20ModuleSwapInfo) {

	stmtLpBalance, err := tx.Prepare(`
INSERT INTO brc20_swap_user_lp_balance(block_height, module_id, pool, pkscript, lp_balance)
VALUES ($1, $2, $3, $4, $5)
`)
	if err != nil {
		log.Panic("PG Statements Wrong: ", err)
	}
	for moduleId, info := range modulesInfoMap {
		for pkscript, tokensMap := range info.UsersLPTokenBalanceMap {
			for pool, balanceData := range tokensMap {
				if _, ok := info.LPTokenUsersBalanceUpdatedMap[pool+pkscript]; !ok {
					continue
				}

				// save balance db
				res, err := stmtLpBalance.Exec(height, moduleId,
					pool,
					pkscript,
					balanceData.String(),
				)
				if err != nil {
					log.Panic("PG Statements Exec Wrong: ", err)
				}
				if _, err := res.RowsAffected(); err != nil {
					log.Panic("PG Affecte Wrong: ", err)
				}
			}
		}
	}

}

func SaveDataToDBModuleTickInfoMap(moduleId string, condStateBalanceDataMap map[string]*model.BRC20ModuleConditionalApproveStateBalance,
	inscriptionsTickerInfoMap, userTokensBalanceData map[string]map[string]*model.BRC20ModuleTokenBalance) {

	// condStateBalanceDataMap
	for ticker, stateBalance := range condStateBalanceDataMap {
		fmt.Printf("  module deposit/withdraw state: %s deposit: %s, match: %s, new: %s, cancel: %s, wait: %s\n",
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

	fmt.Printf("\n")
}

func SaveDataToBRC20DBSwapWithdrawMap(tx *sql.Tx, height uint32,
	inscriptionsValidWithdrawMap map[string]uint32,
) {
	stmtValidWithdraw, err := tx.Prepare(`
INSERT INTO brc20_module_withdrawals(block_height, inscription_id) VALUES ($1, $2)
`)
	if err != nil {
		log.Panic("SaveDataToBRC20DBSwapWithdrawMap, PG Statements Wrong: ", err)
	}

	for inscriptionId, withdrawHeight := range inscriptionsValidWithdrawMap {
		if withdrawHeight != height {
			continue
		}

		res, err := stmtValidWithdraw.Exec(height, inscriptionId)
		if err != nil {
			log.Panic("PG Statements Exec Wrong: ", err)
		}

		if _, err := res.RowsAffected(); err != nil {
			log.Panic("PG Affecte Wrong: ", err)
		}
	}
}
