package controller

import (
	"brc20query/lib/utils"
	"brc20query/logger"
	"brc20query/model"
	"brc20query/service/brc20"
	"encoding/hex"
	"net/http"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"
	"go.uber.org/zap"
)

// GetBRC20TickerHolders
// @Summary Retrieve the list of BRC20 holders by ticker, including information such as address, balance, etc.
// @Tags BRC20
// @Produce  json
// @Param ticker path string true "token ticker" default(ordi)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerHoldersResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/{ticker}/holders [get]
func GetBRC20TickerHolders(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerHolders enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 512 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20TickerHolders(strings.ToLower(ticker), start, limit)
	if err != nil {
		logger.Log.Info("get brc20 holders failed", zap.String("ticker", ticker), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 holders failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerHoldersResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20Status
// @Summary Obtain the BRC20 token list, including information such as the number of holders, total issuance, etc.
// @Tags BRC20
// @Produce  json
// @Param ticker query string false "search brc20 ticker" default()
// @Param complete query string false "complete type(yes/no)" default()
// @Param sort query string false "sort by (holders/deploy/minted/transactions)" default(holders)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerStatusResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/status [get]
func GetBRC20Status(ctx *gin.Context) {
	logger.Log.Info("GetBRC20Status enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 512 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	complete := ctx.DefaultQuery("complete", "")
	ticker := ctx.DefaultQuery("ticker", "")
	tickerHex := strings.ToLower(ctx.DefaultQuery("ticker_hex", ""))
	if len(tickerHex) > 0 {
		tickerStr, err := hex.DecodeString(tickerHex)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", tickerHex))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}

	sortby := ctx.DefaultQuery("sort", "holders")
	if sortby != "holders" && sortby != "deploy" && sortby != "minted" && sortby != "transactions" {
		logger.Log.Info("sortby invalid")
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "sortby invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20Status(strings.ToLower(ticker), complete, sortby, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 status failed", zap.String("ticker", "all"), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 status failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerStatusResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20List
// @Summary Obtain the token list of BRC20
// @Tags BRC20
// @Produce  json
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerListResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/list [get]
func GetBRC20List(ctx *gin.Context) {
	logger.Log.Info("GetBRC20List enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 512 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20List(start, limit)
	if err != nil {
		logger.Log.Info("get brc20 status failed", zap.String("ticker", "all"), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 status failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerListResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20BestHeight
// @Summary Obtain the latest block height of BRC20
// @Tags BRC20
// @Produce  json
// @Success 200 {object} model.Response{data=model.BRC20TickerBestHeightResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/bestheight [get]
func GetBRC20BestHeight(ctx *gin.Context) {
	logger.Log.Info("GetBRC20BestHeight enter")

	if model.GSwap == nil {
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "brc20 not ready"})
		return
	}

	blockid := ""
	blocktime := 0
	last := len(model.GlobalBlocksHash) - 1
	if last >= 0 {
		blockid = utils.GetReversedStringHex(model.GlobalBlocksHash[last])
		blocktime = int(model.GlobalBlocksTime[last])
	}
	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerBestHeightResp{
			Height:     len(model.GlobalBlocksHash) - 1,
			BlockIdHex: blockid,
			BlockTime:  blocktime,
			Total:      len(model.GSwap.InscriptionsTickerInfoMap),
		},
	})
}

// GetBRC20TickerInfo
// @Summary Obtain BRC20 token information, including the number of holders, total circulation, and other information.
// @Tags BRC20
// @Produce  json
// @Param ticker path string true "token ticker" default(ordi)
// @Success 200 {object} model.Response{data=model.BRC20TickerStatusInfo} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/{ticker}/info [get]
func GetBRC20TickerInfo(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerInfo enter")

	if model.GSwap == nil {
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "brc20 not ready"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	nftRsp, err := brc20.GetBRC20TickerInfo(strings.ToLower(ticker))
	if err != nil {
		logger.Log.Info("get brc20 status failed", zap.String("ticker", "all"), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 status failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: nftRsp,
	})
}

// GetBRC20TickersInfo
// @Summary Obtain BRC20 token information in batches, including the number of holders, total issuance, and other information.
// @Tags BRC20
// @Produce  json
// @Param body body []string true "token tickers"
// @Success 200 {object} model.Response{data=[]model.BRC20TickerStatusInfo} "{"code": 0, "data": [{}], "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/tickers-info [get]
func GetBRC20TickersInfo(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickersInfo enter")

	if model.GSwap == nil {
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "brc20 not ready"})
		return
	}

	req := []string{}
	if err := ctx.BindJSON(&req); err != nil {
		logger.Log.Info("Bind json failed", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "json error"})
		return
	}

	tickers := []string{}

	for _, ticker := range req {
		if len(ticker) == 8 {
			tickerStr, err := hex.DecodeString(ticker)
			if err != nil {
				logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
				ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
				return
			}
			ticker = string(tickerStr)
		}
		if len(ticker) != 4 {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}

		tickers = append(tickers, strings.ToLower(ticker))
	}
	if len(tickers) == 0 {
		ctx.JSON(http.StatusOK, model.Response{
			Code: 0,
			Msg:  "ok",
			Data: []string{},
		})
	}

	nftRsps := []*model.BRC20TickerStatusInfo{}
	for _, ticker := range tickers {
		nftRsp, err := brc20.GetBRC20TickerInfo(strings.ToLower(ticker))
		if err != nil {
			logger.Log.Info("get brc20 status failed", zap.String("ticker", "all"), zap.Error(err))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 status failed: " + ticker})
			return
		}

		nftRsps = append(nftRsps, nftRsp)
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: nftRsps,
	})
}

// GetBRC20AllHistoryByHeight
// @History Retrieve the transaction event history of all BRC20 by block height 'height', including information such as address, balance, minting, etc.
// @Tags BRC20
// @Produce  json
// @Param height path int true "Block Height" default(0)
// @Param start query int false "start offset" default(0)
// @Param limit query int false "size of result" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerHistoryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/history-by-height/{height} [get]
func GetBRC20AllHistoryByHeight(ctx *gin.Context) {
	logger.Log.Info("GetBRC20AllHistoryByHeight enter")

	heightString := ctx.Param("height")
	height, err := strconv.Atoi(heightString)
	if err != nil || height < 0 {
		logger.Log.Info("height invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "height invalid"})
		return
	}

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20AllHistoryByHeight(height, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 history failed", zap.Int("height", height), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 history by height failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerHistoryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20AllHistoryByAddress
// @History Retrieve the transaction history of BRC20 by address, including information such as address, balance, and minting.
// @Tags BRC20
// @Produce  json
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerHistoryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/history [get]
func GetBRC20AllHistoryByAddress(ctx *gin.Context) {
	logger.Log.Info("GetBRC20AllHistoryByAddress enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20AllHistoryByAddress(pk, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 history failed", zap.String("address", address), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 history failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerHistoryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20TickerHistory
// @History Get the BRC20 transaction history by ticker, including information such as address, balance, minting, etc.
// @Tags BRC20
// @Produce  json
// @Param type query string false "history type(inscribe-deploy/inscribe-mint/inscribe-transfer/transfer/send/receive)" default()
// @Param ticker path string true "token ticker" default(ordi)
// @Param height query int false "start offset" default(0)
// @Param start query int false "start offset" default(0)
// @Param limit query int false "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerHistoryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /brc20/{ticker}/history [get]
func GetBRC20TickerHistory(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerHistory enter")

	heightString := ctx.DefaultQuery("height", "0")
	height, err := strconv.Atoi(heightString)
	if err != nil || height < 0 {
		logger.Log.Info("height invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "height invalid"})
		return
	}

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	historyType := ctx.DefaultQuery("type", "")

	total, nftsRsp, err := brc20.GetBRC20TickerHistory(historyType, strings.ToLower(ticker), height, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 history failed", zap.String("ticker", ticker), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 history failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerHistoryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20SummaryByAddress
// @Summary Obtain the BRC20 holding list by address, including information such as ticker, balance, etc.
// @Tags BRC20
// @Produce  json
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TokenSummaryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/summary [get]
func GetBRC20SummaryByAddress(ctx *gin.Context) {
	logger.Log.Info("GetBRC20SummaryByAddress enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20SummaryByAddress(pk, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 summary failed", zap.String("address", address), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 summary failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TokenSummaryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20SummaryByAddressAndHeight
// @Summary Obtain the BRC20 holding list by address, including information such as ticker, balance, etc.
// @Tags BRC20
// @Produce  json
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param height path int true "Block Height" default(0)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TokenSummaryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/summary-by-height/{height} [get]
func GetBRC20SummaryByAddressAndHeight(ctx *gin.Context) {
	logger.Log.Info("GetBRC20SummaryByAddressAndHeight enter")

	heightString := ctx.Param("height")
	height, err := strconv.Atoi(heightString)
	if err != nil || height < 0 {
		logger.Log.Info("height invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "height invalid"})
		return
	}

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	total, nftsRsp, err := brc20.GetBRC20SummaryByAddressAndHeight(pk, height, start, limit)
	if err != nil {
		logger.Log.Info("get brc20 summary by height failed", zap.String("address", address), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 summary by height failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TokenSummaryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20TickerHistoryByAddress
// @History Obtain the BRC20 holding list by address, including information such as ticker, balance, etc.
// @Tags BRC20
// @Produce  json
// @Param type query string false "history type(inscribe-deploy/inscribe-mint/inscribe-transfer/transfer/send/receive)" default()
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param ticker path string true "token ticker" default(ordi)
// @Param start query int true "start offset" default(0)
// @Param limit query int true "number of nft" default(10)
// @Success 200 {object} model.Response{data=model.BRC20TickerHistoryResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/{ticker}/history [get]
func GetBRC20TickerHistoryByAddress(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerHistoryByAddress enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 10240 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	historyType := ctx.DefaultQuery("type", "")

	total, nftsRsp, err := brc20.GetBRC20TickerHistoryByAddress(pk, historyType, strings.ToLower(ticker), start, limit)
	if err != nil {
		logger.Log.Info("get brc20 summary failed", zap.String("address", address), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 summary failed"})
		return
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerHistoryResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}

// GetBRC20TickerInfoByAddress
// @Summary Retrieve the BRC20 token by address, including available balance, transferable balance, number of transferable Inscriptions, and the first few Inscriptions, etc.
// @Tags BRC20
// @Produce  json
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param ticker path string true "token ticker" default(ordi)
// @Success 200 {object} model.Response{data=model.BRC20TickerStatusInfoOfAddressResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/{ticker}/info [get]
func GetBRC20TickerInfoByAddress(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerInfoByAddress enter")

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	nftRsp, err := brc20.GetBRC20TickerInfoByAddress(pk, strings.ToLower(ticker))
	if err != nil {
		logger.Log.Info("get brc20 info by address failed", zap.String("ticker", ticker), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 info failed"})
		return
	}

	for _, resp := range nftRsp.TransferableInscriptions {
		if model.GBestHeight > 0 {
			if resp.Height == model.MEMPOOL_HEIGHT {
				resp.Confirmations = 0
			} else {
				resp.Confirmations = model.GBestHeight - int(resp.Height) + 1
			}
		}
	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: nftRsp,
	})
}

// GetBRC20TickerTransferableInscriptionsByAddress
// @Summary Retrieve BRC20 Inscriptions list by address
// @Tags BRC20
// @Produce  json
// @Param address path string true "Address" default(17SkEw2md5avVNyYgj6RiXuQKNwkXaxFyQ)
// @Param ticker path string true "token ticker" default(ordi)
// @Param start query int false "start offset" default(0)
// @Param limit query int false "number of nft" default(10)
// @Param invalid query string false "number of nft" default(false)
// @Success 200 {object} model.Response{data=model.BRC20TickerInscriptionsResp} "{"code": 0, "data": {}, "msg": "ok"}"
// @Security BearerAuth
// @Router /address/{address}/brc20/{ticker}/transferable-inscriptions [get]
func GetBRC20TickerTransferableInscriptionsByAddress(ctx *gin.Context) {
	logger.Log.Info("GetBRC20TickerTransferableInscriptionsByAddress enter")

	startString := ctx.DefaultQuery("start", "0")
	start, err := strconv.Atoi(startString)
	if err != nil || start < 0 {
		logger.Log.Info("start invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "start invalid"})
		return
	}

	limitString := ctx.DefaultQuery("limit", "10")
	limit, err := strconv.Atoi(limitString)
	if err != nil || limit < 1 || limit > 512 {
		logger.Log.Info("limit invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "limit invalid"})
		return
	}

	address := ctx.Param("address")
	// check
	pk, err := utils.GetPkScriptByAddress(address)
	if err != nil {
		logger.Log.Info("address invalid", zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "address invalid"})
		return
	}

	ticker := ctx.Param("ticker")
	if len(ticker) == 8 {
		tickerStr, err := hex.DecodeString(ticker)
		if err != nil {
			logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
			ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
			return
		}
		ticker = string(tickerStr)
	}
	if len(ticker) != 4 {
		logger.Log.Info("ticker invalid", zap.String("ticker", ticker))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "ticker invalid"})
		return
	}

	invalid := ctx.DefaultQuery("invalid", "false")
	total, nftsRsp, err := brc20.GetBRC20TickerTransferableInscriptionsByAddress(pk, strings.ToLower(ticker), start, limit, invalid == "true")
	if err != nil {
		logger.Log.Info("get brc20 summary failed", zap.String("address", address), zap.Error(err))
		ctx.JSON(http.StatusOK, model.Response{Code: -1, Msg: "get brc20 summary failed"})
		return
	}

	for idx, resp := range nftsRsp {
		if model.GBestHeight > 0 {
			if resp.Height == model.MEMPOOL_HEIGHT {
				nftsRsp[idx].Confirmations = 0
			} else {
				nftsRsp[idx].Confirmations = model.GBestHeight - int(resp.Height) + 1
			}
		}

	}

	ctx.JSON(http.StatusOK, model.Response{
		Code: 0,
		Msg:  "ok",
		Data: &model.BRC20TickerInscriptionsResp{
			Height: len(model.GlobalBlocksHash) - 1,
			Total:  total,
			Start:  start,
			Detail: nftsRsp,
		},
	})
}
