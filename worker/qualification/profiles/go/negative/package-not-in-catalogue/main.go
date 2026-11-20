// Fixture: the package catalogue is unqualified and empty, so no module is accepted.
package main

import "github.com/google/uuid"

func main() { _ = uuid.New }
