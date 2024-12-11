package indexer

import (
	"bytes"
	"log"

	"github.com/unisat-wallet/libbrc20-indexer/conf"
	"github.com/unisat-wallet/libbrc20-indexer/constant"
	"github.com/unisat-wallet/libbrc20-indexer/model"
)

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

// ProcessUpdateLatestBRC20Loop
func (g *BRC20ModuleIndexer) ProcessUpdateLatestBRC20Loop(brc20Datas, brc20DatasDump chan interface{}) {
	if brc20Datas == nil {
		return
	}

	g.Durty = false
	for dataIn := range brc20Datas {

		for {
			data := dataIn.(*model.InscriptionBRC20Data)

			// update latest height
			g.BestHeight = data.Height

			// is sending transfer
			if data.IsTransfer {

				if data.Height < conf.ENABLE_SWAP_WITHDRAW_HEIGHT {
					// module conditional approve
					if condApproveInfo, isInvalid := g.GetConditionalApproveInfoByKey(data.CreateIdxKey); condApproveInfo != nil {
						if err := g.ProcessConditionalApprove(data, condApproveInfo, isInvalid); err != nil {
							log.Printf("process conditional approve move failed: %s", err)
						} else {
							g.Durty = true
						}
						break
					}
				}

				// not first move
				if data.Sequence != 1 {
					break
				}

				// transfer

				if _, ok := g.InscriptionsTransferRemoveMap[data.CreateIdxKey]; ok {
					break
				}
				if transferInfo, isInvalid := g.GetTransferInfoByKey(data.CreateIdxKey); transferInfo != nil {
					g.InscriptionsTransferRemoveMap[data.CreateIdxKey] = data.Height
					g.Durty = true

					if err := g.ProcessTransfer(data, transferInfo, isInvalid); err != nil {
						log.Printf("process transfer move failed: %s", err)
					}
					break
				}

				// module approve
				if approveInfo, isInvalid := g.GetApproveInfoByKey(data.CreateIdxKey); approveInfo != nil {
					g.InscriptionsApproveRemoveMap[data.CreateIdxKey] = data.Height
					g.Durty = true

					if err := g.ProcessApprove(data, approveInfo, isInvalid); err != nil {
						log.Printf("process approve move failed: %s", err)
					}
					break
				}

				// module withdraw
				if withdrawInfo := g.GetWithdrawInfoByKey(data.CreateIdxKey); withdrawInfo != nil {
					g.InscriptionsWithdrawRemoveMap[data.CreateIdxKey] = data.Height
					g.Durty = true

					if err := g.ProcessWithdraw(data, withdrawInfo); err != nil {
						log.Printf("process withdraw move failed: %s", err)
					} else {
						g.InscriptionsValidWithdrawMap[withdrawInfo.Data.GetInscriptionId()] = data.Height
					}
					break
				}

				// module commit
				if commitFrom, isInvalid := g.GetCommitInfoByKey(data.CreateIdxKey); commitFrom != nil {
					g.InscriptionsCommitRemoveMap[data.CreateIdxKey] = data.Height
					g.Durty = true

					if err := g.ProcessCommit(commitFrom, data, isInvalid); err != nil {
						log.Printf("process commit move failed: %s", err)
					}
					break
				}

				break
			}

			// inscribe as fee
			if data.Satoshi == 0 {
				break
			}

			if ok := isJson(data.ContentBody); !ok {
				// log.Println("not json")
				break
			}

			// protocal, lower case only
			body := new(model.InscriptionBRC20ProtocalContent)
			if err := body.Unmarshal(data.ContentBody); err != nil {
				// log.Println("Unmarshal failed", err, string(data.ContentBody))
				break
			}

			// is inscribe deploy/mint/transfer
			if body.Proto != constant.BRC20_P &&
				body.Proto != constant.BRC20_P_MODULE &&
				body.Proto != constant.BRC20_P_SWAP {
				// log.Println("not proto")
				break
			}

			var process func(*model.InscriptionBRC20Data) error
			if body.Proto == constant.BRC20_P && body.Operation == constant.BRC20_OP_DEPLOY {
				process = g.ProcessDeploy
			} else if body.Proto == constant.BRC20_P && body.Operation == constant.BRC20_OP_MINT {
				process = g.ProcessMint
			} else if body.Proto == constant.BRC20_P && body.Operation == constant.BRC20_OP_TRANSFER {
				process = g.ProcessInscribeTransfer
			} else if body.Proto == constant.BRC20_P_MODULE && body.Operation == constant.BRC20_OP_MODULE_DEPLOY {
				process = g.ProcessCreateModule
			} else if body.Proto == constant.BRC20_P_MODULE && body.Operation == constant.BRC20_OP_MODULE_WITHDRAW {
				process = g.ProcessInscribeWithdraw
			} else if body.Proto == constant.BRC20_P_SWAP && body.Operation == constant.BRC20_OP_SWAP_APPROVE {
				process = g.ProcessInscribeApprove
			} else if body.Proto == constant.BRC20_P_SWAP && body.Operation == constant.BRC20_OP_SWAP_CONDITIONAL_APPROVE {
				process = g.ProcessInscribeConditionalApprove
			} else if body.Proto == constant.BRC20_P_SWAP && body.Operation == constant.BRC20_OP_SWAP_COMMIT {
				process = g.ProcessInscribeCommit
			} else {
				break
			}

			if err := process(data); err != nil {
				if body.Operation == constant.BRC20_OP_MINT {
					if conf.DEBUG {
						log.Printf("(%d) process failed: %s", g.BestHeight, err)
					}
				} else {
					log.Printf("(%d) process failed: %s", g.BestHeight, err)
				}
			} else {
				// caution: need update but durty == false
				g.Durty = true
			}
			break
		}
		if brc20DatasDump != nil {
			brc20DatasDump <- dataIn
		}
	}

	for _, holdersBalanceMap := range g.TokenUsersBalanceData {
		for key, balance := range holdersBalanceMap {
			if balance.AvailableBalance.Sign() == 0 && balance.TransferableBalance.Sign() == 0 {
				delete(holdersBalanceMap, key)
			}
		}
	}
	if !g.Durty {
		return
	}

	log.Printf("process brc20 (%d). ticker: %d, users: %d, tokens: %d, validInscription: %d, validTransfer: %d, invalidTransfer: %d, history: %d",
		g.BestHeight,
		len(g.InscriptionsTickerInfoMap),
		len(g.UserTokensBalanceData),
		len(g.TokenUsersBalanceData),

		len(g.InscriptionsValidBRC20DataMap),

		len(g.InscriptionsValidTransferMap),
		len(g.InscriptionsInvalidTransferMap),

		g.HistoryCount,
	)

	nswap := 0
	for _, m := range g.ModulesInfoMap {
		nswap += len(m.SwapPoolTotalBalanceDataMap)
	}

	nuser := 0
	for _, m := range g.ModulesInfoMap {
		nuser += len(m.UsersTokenBalanceDataMap)
	}

	if nuser > 0 {
		log.Printf("process swap (%d). module: %d, swap: %d, users: %d, validApprove: %d, invalidApprove: %d, validCommit: %d, invalidCommit: %d",
			g.BestHeight,
			len(g.ModulesInfoMap),
			nswap,
			nuser,

			len(g.InscriptionsValidApproveMap),
			len(g.InscriptionsInvalidApproveMap),

			len(g.InscriptionsValidCommitMap),
			len(g.InscriptionsInvalidCommitMap),
		)
	}
}

func (g *BRC20ModuleIndexer) Init() {
	g.initBRC20()
	g.initModule()
}
