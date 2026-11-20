package bindings

//go:wasmimport falcone:host/logging@0.2.0 log
func log(level uint32, message *byte, messageLen uint32, fields *uint32, fieldCount uint32, ret *area)

// Level is falcone:host/logging level: debug, info, warn, error.
type Level uint32

const (
	Debug Level = iota
	Info
	Warn
	Error
)

// Field is one structured log field. The host redacts sensitive fields.
type Field struct{ Name, Value string }

// rawField is the canonical-ABI record { name: string, value: string }: 16 bytes, align 4.
type rawField struct {
	name, nameLen, value, valueLen uint32
}

// Log writes one structured log line scoped by the verified invocation context.
func Log(level Level, message string, fields ...Field) error {
	var ret area
	raw := make([]rawField, len(fields))
	for i, f := range fields {
		np, nn := stringData(f.Name)
		vp, vn := stringData(f.Value)
		raw[i] = rawField{address(np), nn, address(vp), vn}
	}
	// The list is passed as the address of its first word; an empty list lowers as nil, 0.
	var first *uint32
	if len(raw) > 0 {
		first = &raw[0].name
	}
	mp, mn := stringData(message)
	log(uint32(level), mp, mn, first, uint32(len(raw)), &ret)
	return resultUnit(&ret)
}
