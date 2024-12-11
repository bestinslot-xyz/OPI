package utils

import (
	"encoding/hex"
	"errors"
	"strconv"
	"strings"
)

func VerifyInscriptionId(inscriptionId string) (err error) {
	if len(inscriptionId) > 64+1+12 {
		return errors.New("inscriptionId too long")
	}

	parts := strings.Split(inscriptionId, "i")
	if len(parts) != 2 || len(parts[0]) != 64 {
		return errors.New("inscriptionId invalid, without 'i'")
	}

	if _, err := hex.DecodeString(parts[0]); err != nil {
		return errors.New("inscriptionId invalid, not hex")
	}

	if idx, err := strconv.Atoi(parts[1]); err != nil || idx < 0 {
		return errors.New("inscriptionId invalid, idx")
	}

	return nil
}
