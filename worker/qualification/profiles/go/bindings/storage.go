package bindings

//go:wasmimport falcone:host/storage@0.2.0 open-object
func openObject(key *byte, keyLen uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 read-object
func readObject(handle uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 write-object
func writeObject(handle uint32, data *byte, dataLen uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 [resource-drop]object
func dropObject(handle uint32)

//go:wasmimport falcone:host/storage@0.2.0 open-checkpoint
func openCheckpoint(key *byte, keyLen uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 read-checkpoint
func readCheckpoint(handle uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 write-checkpoint
func writeCheckpoint(handle uint32, data *byte, dataLen uint32, ret *area)

//go:wasmimport falcone:host/storage@0.2.0 [resource-drop]checkpoint
func dropCheckpoint(handle uint32)

// Object is an execution-scoped authorized object handle. Authorization comes only from the
// verified invocation context, never from the key.
type Object struct{ handle uint32 }

// OpenObject opens a key relative to the host-authorized storage binding.
func OpenObject(key string) (Object, error) {
	var ret area
	p, n := stringData(key)
	openObject(p, n, &ret)
	handle, err := resultHandle(&ret)
	return Object{handle}, err
}

// Read returns the object's bytes.
func (o Object) Read() ([]byte, error) {
	var ret area
	readObject(o.handle, &ret)
	return resultList(&ret)
}

// Write replaces the object's bytes.
func (o Object) Write(data []byte) error {
	var ret area
	p, n := bytesData(data)
	writeObject(o.handle, p, n, &ret)
	return resultUnit(&ret)
}

// Close drops the handle.
func (o Object) Close() { dropObject(o.handle) }

// Checkpoint is an execution-scoped checkpoint handle, bound by the host to the revision,
// schema, input digest and fenced attempt.
type Checkpoint struct{ handle uint32 }

func OpenCheckpoint(key string) (Checkpoint, error) {
	var ret area
	p, n := stringData(key)
	openCheckpoint(p, n, &ret)
	handle, err := resultHandle(&ret)
	return Checkpoint{handle}, err
}

func (c Checkpoint) Read() ([]byte, error) {
	var ret area
	readCheckpoint(c.handle, &ret)
	return resultList(&ret)
}

func (c Checkpoint) Write(data []byte) error {
	var ret area
	p, n := bytesData(data)
	writeCheckpoint(c.handle, p, n, &ret)
	return resultUnit(&ret)
}

func (c Checkpoint) Close() { dropCheckpoint(c.handle) }
