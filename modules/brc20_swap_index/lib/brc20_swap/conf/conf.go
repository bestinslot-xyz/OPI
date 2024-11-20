package conf

import (
	"github.com/btcsuite/btcd/chaincfg"
)

var (
	DEBUG                                    = false
	MODULE_SWAP_SOURCE_INSCRIPTION_ID        = "93ce120ff87364c261a534fea4c39196a615f449412fb3547a185d92306a39b8i0"
	GlobalNetParams                          = &chaincfg.MainNetParams
	TICKS_ENABLED                            = ""
	ENABLE_SELF_MINT_HEIGHT           uint32 = 837090
	ENABLE_SWAP_WITHDRAW_HEIGHT       uint32 = 847090 // fixme: dummy height
)
