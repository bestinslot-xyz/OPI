package decimal

import (
	"database/sql/driver"
	"errors"
	"fmt"
	"math"
	"math/big"
	"strings"
)

const MAX_PRECISION = 18

var MAX_PRECISION_STRING = "18"

var precisionFactor [19]*big.Int = [19]*big.Int{
	new(big.Int).Exp(big.NewInt(10), big.NewInt(0), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(1), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(2), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(3), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(4), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(5), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(6), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(7), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(8), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(9), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(10), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(11), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(12), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(13), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(14), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(15), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(16), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(17), nil),
	new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil),
}

// Decimal represents a fixed-point decimal number with 18 decimal places
type Decimal struct {
	Precition uint
	Val       *big.Int
}

// Value implements the driver.Valuer interface for the Decimal type
func (d *Decimal) Value() (driver.Value, error) {
	if d == nil {
		return nil, nil
	}
	return d.String(), nil
}

// Scan implements the sql.Scanner interface for the Decimal type
func (d *Decimal) Scan(value interface{}) error {
	if value == nil {
		return nil
	}

	switch v := value.(type) {
	case string:
		dec, err := NewDecimalFromString(v, MAX_PRECISION)
		if err != nil {
			return err
		}
		d.Precition = dec.Precition
		d.Val = dec.Val
	case []byte:
		dec, err := NewDecimalFromString(string(v), MAX_PRECISION)
		if err != nil {
			return err
		}
		d.Precition = dec.Precition
		d.Val = dec.Val
	default:
		return errors.New("unsupported value type")
	}

	return nil
}

func NewDecimal(v uint64, p uint) *Decimal {
	if p > MAX_PRECISION {
		p = MAX_PRECISION
	}
	return &Decimal{Precition: p, Val: new(big.Int).SetUint64(v)}
}

func NewDecimalCopy(other *Decimal) *Decimal {
	if other == nil {
		return nil
	}
	return &Decimal{Precition: other.Precition, Val: new(big.Int).Set(other.Val)}
}

// NewDecimalFromString creates a Decimal instance from a string
func NewDecimalFromString(s string, maxPrecision int) (*Decimal, error) {
	if s == "" {
		return nil, errors.New("empty string")
	}

	parts := strings.Split(s, ".")
	if len(parts) > 2 {
		return nil, fmt.Errorf("invalid decimal format: %s", s)
	}

	integerPartStr := parts[0]
	if integerPartStr == "" || integerPartStr[0] == '+' {
		return nil, errors.New("empty integer")
	}

	integerPart, ok := new(big.Int).SetString(parts[0], 10)
	if !ok {
		return nil, fmt.Errorf("invalid integer format: %s", parts[0])
	}

	currPrecision := 0
	decimalPart := big.NewInt(0)
	if len(parts) == 2 {
		decimalPartStr := parts[1]
		if decimalPartStr == "" || decimalPartStr[0] == '-' || decimalPartStr[0] == '+' {
			return nil, errors.New("empty decimal")
		}

		currPrecision = len(decimalPartStr)
		if currPrecision > maxPrecision {
			return nil, fmt.Errorf("decimal exceeds maximum precision(%d): %s", maxPrecision, s)
		}
		n := maxPrecision - currPrecision
		for i := 0; i < n; i++ {
			decimalPartStr += "0"
		}
		decimalPart, ok = new(big.Int).SetString(decimalPartStr, 10)
		if !ok || decimalPart.Sign() < 0 {
			return nil, fmt.Errorf("invalid decimal format: %s", parts[0])
		}
	}

	value := new(big.Int).Mul(integerPart, precisionFactor[maxPrecision])
	if value.Sign() < 0 {
		value = value.Sub(value, decimalPart)
	} else {
		value = value.Add(value, decimalPart)
	}

	return &Decimal{Precition: uint(maxPrecision), Val: value}, nil
}

func MustNewDecimalFromString(s string, maxPrecision int) *Decimal {
	parts := strings.Split(s, ".")
	if len(parts) == 2 {
		s = strings.TrimRight(s, "0")
		s = strings.TrimRight(s, ".")
	}
	val, err := NewDecimalFromString(s, maxPrecision)
	if err != nil {
		panic(err)
	}
	return val
}

// String returns the string representation of a Decimal instance
func (d *Decimal) String() string {
	if d == nil {
		return "0"
	}
	value := new(big.Int).Abs(d.Val)
	quotient, remainder := new(big.Int).QuoRem(value, precisionFactor[d.Precition], new(big.Int))
	sign := ""
	if d.Val.Sign() < 0 {
		sign = "-"
	}
	if remainder.Sign() == 0 {
		return fmt.Sprintf("%s%s", sign, quotient.String())
	}
	decimalPart := fmt.Sprintf("%0*d", d.Precition, remainder)
	decimalPart = strings.TrimRight(decimalPart, "0")
	return fmt.Sprintf("%s%s.%s", sign, quotient.String(), decimalPart)
}

// Add adds two Decimal instances and returns a new Decimal instance
func (d *Decimal) Add(other *Decimal) *Decimal {
	if d == nil && other == nil {
		return nil
	}
	if other == nil {
		value := new(big.Int).Set(d.Val)
		return &Decimal{Precition: d.Precition, Val: value}
	}
	if d == nil {
		value := new(big.Int).Set(other.Val)
		return &Decimal{Precition: other.Precition, Val: value}
	}
	if d.Precition != other.Precition {
		panic("precition not match")
	}
	value := new(big.Int).Add(d.Val, other.Val)
	return &Decimal{Precition: d.Precition, Val: value}
}

// Sub subtracts two Decimal instances and returns a new Decimal instance
func (d *Decimal) Sub(other *Decimal) *Decimal {
	if d == nil && other == nil {
		return nil
	}
	if other == nil {
		value := new(big.Int).Set(d.Val)
		return &Decimal{Precition: d.Precition, Val: value}
	}
	if d == nil {
		value := new(big.Int).Neg(other.Val)
		return &Decimal{Precition: other.Precition, Val: value}
	}
	if d.Precition != other.Precition {
		panic(fmt.Sprintf("precition not match, (%d != %d)", d.Precition, other.Precition))
	}
	value := new(big.Int).Sub(d.Val, other.Val)
	return &Decimal{Precition: d.Precition, Val: value}
}

// Mul muls two Decimal instances and returns a new Decimal instance
func (d *Decimal) Mul(other *Decimal) *Decimal {
	if d == nil || other == nil {
		return nil
	}
	value := new(big.Int).Mul(d.Val, other.Val)
	// value := new(big.Int).Div(value0, precisionFactor[other.Precition])
	return &Decimal{Precition: d.Precition, Val: value}
}

// Sqrt muls two Decimal instances and returns a new Decimal instance
func (d *Decimal) Sqrt() *Decimal {
	if d == nil {
		return nil
	}
	// value0 := new(big.Int).Mul(d.Value, precisionFactor[d.Precition])
	value := new(big.Int).Sqrt(d.Val)
	return &Decimal{Precition: MAX_PRECISION, Val: value}
}

// Div divs two Decimal instances and returns a new Decimal instance
func (d *Decimal) Div(other *Decimal) *Decimal {
	if d == nil || other == nil {
		return nil
	}
	// value0 := new(big.Int).Mul(d.Value, precisionFactor[other.Precition])
	value := new(big.Int).Div(d.Val, other.Val)
	return &Decimal{Precition: d.Precition, Val: value}
}

func (d *Decimal) NewPrecition(p uint) *Decimal {
	if d == nil {
		return nil
	}

	c := int64(d.Precition) - int64(p)
	if c == 0 {
		return d
	} else if c < 0 {
		panic(fmt.Errorf("precition must be less"))
	}

	val := new(big.Int).Div(d.Val, precisionFactor[c])
	return &Decimal{Precition: p, Val: val}
}

func (d *Decimal) Cmp(other *Decimal) int {
	if d == nil && other == nil {
		return 0
	}
	if other == nil {
		return d.Val.Sign()
	}
	if d == nil {
		return -other.Val.Sign()
	}
	if d.Precition != other.Precition {
		panic(fmt.Sprintf("precition not match, (%d != %d)", d.Precition, other.Precition))
	}
	return d.Val.Cmp(other.Val)
}

func (d *Decimal) CmpAlign(other *Decimal) int {
	if d == nil && other == nil {
		return 0
	}
	if other == nil {
		return d.Val.Sign()
	}
	if d == nil {
		return -other.Val.Sign()
	}
	return d.Val.Cmp(other.Val)
}

func (d *Decimal) Sign() int {
	if d == nil {
		return 0
	}
	return d.Val.Sign()
}

func (d *Decimal) IsOverflowUint64() bool {
	if d == nil {
		return false
	}

	integerPart := new(big.Int).SetUint64(math.MaxUint64)
	value := new(big.Int).Mul(integerPart, precisionFactor[d.Precition])
	if d.Val.Cmp(value) > 0 {
		return true
	}
	return false
}

func (d *Decimal) GetMaxUint64() *Decimal {
	if d == nil {
		return nil
	}
	integerPart := new(big.Int).SetUint64(math.MaxUint64)
	value := new(big.Int).Mul(integerPart, precisionFactor[d.Precition])
	return &Decimal{Precition: d.Precition, Val: value}
}

func (d *Decimal) Float64() float64 {
	if d == nil {
		return 0
	}
	value := new(big.Int).Abs(d.Val)
	quotient, remainder := new(big.Int).QuoRem(value, precisionFactor[d.Precition], new(big.Int))
	f := float64(quotient.Uint64()) + float64(remainder.Uint64())/math.MaxFloat64
	if d.Val.Sign() < 0 {
		return -f
	}
	return f
}
