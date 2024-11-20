package main

import (
	"brc20query/controller"
	_ "brc20query/docs"
	"brc20query/logger"
	"brc20query/service/brc20"
	"context"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	cache "github.com/chenyahui/gin-cache"
	"github.com/chenyahui/gin-cache/persist"

	"github.com/gin-contrib/gzip"
	"github.com/gin-contrib/pprof"
	ginzap "github.com/gin-contrib/zap"
	"github.com/gin-gonic/gin"
	swaggerFiles "github.com/swaggo/files"
	ginSwagger "github.com/swaggo/gin-swagger"
	"go.uber.org/zap"
)

var (
	listen_address          = os.Getenv("LISTEN")
	basePath                = os.Getenv("BASE_PATH")
	startHeightBRC20Process = os.Getenv("BRC20_PROCESS_AFTER_HEIGHT")
	endHeightBRC20Process   = os.Getenv("BRC20_PROCESS_BEFORE_HEIGHT")
)

var (
	brc20SwapReady = false
)

func SetSwagTitle(title string) func(*ginSwagger.Config) {
	return func(c *ginSwagger.Config) {
		c.Title = title
	}
}

// @title BRC20 Query Spec
// @version 2.0
// @description API definition for BRC20Query  APIs

// @contact.name brc20query
// @contact.url https://github.com/unisat/brc20query
// @contact.email jiedohh@gmail.com

// @license.name MIT License
// @license.url https://opensource.org/licenses/MIT

// @securityDefinitions.apikey BearerAuth
// @in header
// @name Authorization
func main() {
	router := gin.New()
	router.Use(ginzap.Ginzap(logger.Log, time.RFC3339, true))
	router.Use(ginzap.RecoveryWithZap(logger.Log, true))

	router.Use(gzip.Gzip(gzip.DefaultCompression, gzip.WithDecompressFn(gzip.DefaultDecompressHandle)))

	router.UseRawPath = true
	router.UnescapePathValues = true

	// go get -u github.com/swaggo/swag/cmd/swag@v1.6.7

	store := persist.NewMemoryStore(time.Second)

	router.GET("/swagger/*any", ginSwagger.WrapHandler(swaggerFiles.Handler,
		ginSwagger.URL(basePath+"/swagger/doc.json"),
		SetSwagTitle("BRC20")))

	pprof.Register(router)

	// for brc20
	brc20API := router.Group("/")
	{
		// brc20
		brc20API.GET("/brc20/bestheight",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20BestHeight)
		brc20API.GET("/brc20/list",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20List)

		brc20API.GET("/brc20/status",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20Status)

		brc20API.GET("/brc20/history-by-height/:height",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20AllHistoryByHeight)
		brc20API.GET("/address/:address/brc20/history",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20AllHistoryByAddress)
		brc20API.GET("/brc20/:ticker/history",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20TickerHistory)
		brc20API.GET("/address/:address/brc20/:ticker/history",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20TickerHistoryByAddress)

		brc20API.POST("/brc20/tickers-info",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20TickersInfo)

		brc20API.GET("/brc20/:ticker/info",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20TickerInfo)
		brc20API.GET("/brc20/:ticker/holders",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20TickerHolders)

		brc20API.GET("/address/:address/brc20/summary",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20SummaryByAddress)
		brc20API.GET("/address/:address/brc20/summary-by-height/:height",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20SummaryByAddressAndHeight)

		brc20API.GET("/address/:address/brc20/:ticker/info", controller.GetBRC20TickerInfoByAddress)
		brc20API.GET("/address/:address/brc20/:ticker/transferable-inscriptions", controller.GetBRC20TickerTransferableInscriptionsByAddress)

		// brc20 swap module
		brc20API.GET("/brc20-module/:module/history",
			cache.CacheByRequestURI(store, 2*time.Second), controller.GetBRC20ModuleHistory)

		brc20API.GET("/brc20-module/:module/address/:address/brc20/:ticker/info", controller.GetBRC20ModuleTickerInfoByAddress)

		brc20API.GET("/brc20-module/inscription/info/:inscriptionId", controller.GetBRC20ModuleInscriptionInfo)

		brc20API.POST("/brc20-module/verify-commit", controller.BRC20ModuleVerifySwapCommitContent)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// brc20 swap
	go func() {
		endHeight, _ := strconv.Atoi(endHeightBRC20Process)
		startHeight, _ := strconv.Atoi(startHeightBRC20Process)
		brc20.ProcessUpdateLatestBRC20SwapInit(ctx, startHeight, endHeight)
		brc20SwapReady = true
	}()

	logger.Log.Info("LISTEN:",
		zap.String("address", listen_address),
	)
	svr := &http.Server{
		Addr:    listen_address,
		Handler: router,
	}

	go func() {
		for {
			if !brc20SwapReady {
				time.Sleep(time.Second * 2)
				continue
			}
			break
		}
		err := svr.ListenAndServe()
		if err != nil && err != http.ErrServerClosed {
			logger.Log.Fatal("ListenAndServe:",
				zap.Error(err),
			)
		}
	}()

	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	<-quit
	logger.Log.Info("Shutdown Server ...")
	cancel()

	{
		timeout := time.Duration(1) * time.Second
		ctx, cancel := context.WithTimeout(context.Background(), timeout)
		defer cancel()

		if err := svr.Shutdown(ctx); err != nil {
			logger.Log.Fatal("Shutdown:",
				zap.Error(err),
			)
		}
	}
}
