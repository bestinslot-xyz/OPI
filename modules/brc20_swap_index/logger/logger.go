package logger

import (
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"
)

var (
	Log *zap.Logger
)

func init() {
	loglevel := "debug"

	level := zapcore.DebugLevel
	if loglevel == "info" {
		level = zapcore.InfoLevel
	} else if loglevel == "error" {
		level = zapcore.ErrorLevel
	}

	enc := zap.NewProductionEncoderConfig()
	enc.EncodeTime = zapcore.RFC3339NanoTimeEncoder

	Log, _ = zap.Config{
		Encoding:          "json",
		Level:             zap.NewAtomicLevelAt(level),
		EncoderConfig:     enc,
		DisableCaller:     true,
		DisableStacktrace: true,
		OutputPaths:       []string{"stdout"},
	}.Build()
}

func SyncLog() {
	Log.Sync()
}
