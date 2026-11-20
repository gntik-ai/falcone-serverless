// Minimal handle() example: it reads a rate-limit bucket from storage, writes the updated
// bucket back and publishes an access event. Imports are falcone:host interfaces only.
package main

import (
	"encoding/json"
	"strconv"

	"falcone.invalid/profile/bindings"
)

const topic = "console.openapi.spec.accessed"

const rateLimited = `{"statusCode":429,"headers":{"Retry-After":"60"},"body":{"code":"RATE_LIMITED","message":"Too many requests"}}`

const accepted = `{"statusCode":200,"body":{"ok":true}}`

type request struct {
	WorkspaceID string `json:"workspaceId"`
	TenantID    string `json:"tenantId"`
	RequesterID string `json:"requesterId"`
	SpecVersion string `json:"specVersion"`
	Format      string `json:"format"`
	Now         int64  `json:"now"`
	Limit       int64  `json:"limit"`
}

type bucket struct {
	Minute int64 `json:"minute"`
	Count  int64 `json:"count"`
}

type event struct {
	EventType   string `json:"eventType"`
	Timestamp   string `json:"timestamp"`
	WorkspaceID string `json:"workspaceId"`
	TenantID    string `json:"tenantId"`
	SpecVersion string `json:"specVersion"`
	RequesterID string `json:"requesterId"`
	Format      string `json:"format"`
}

func init() { bindings.Handle(handle) }

func main() {}

func pad(value int64, width int) string {
	s := strconv.FormatInt(value, 10)
	for len(s) < width {
		s = "0" + s
	}
	return s
}

// isoTime formats Unix milliseconds as an ISO 8601 UTC timestamp without the time package.
func isoTime(ms int64) string {
	seconds, millis := ms/1000, ms%1000
	z := seconds/86400 + 719468
	era := z / 146097
	doe := z - era*146097
	yoe := (doe - doe/1460 + doe/36524 - doe/146096) / 365
	year := yoe + era*400
	doy := doe - (365*yoe + yoe/4 - yoe/100)
	mp := (5*doy + 2) / 153
	day := doy - (153*mp+2)/5 + 1
	month := mp + 3
	if month > 12 {
		month -= 12
	}
	if month <= 2 {
		year++
	}
	rest := seconds % 86400
	return pad(year, 4) + "-" + pad(month, 2) + "-" + pad(day, 2) + "T" +
		pad(rest/3600, 2) + ":" + pad(rest%3600/60, 2) + ":" + pad(rest%60, 2) + "." + pad(millis, 3) + "Z"
}

func handle(params []byte) ([]byte, *bindings.HandlerError) {
	var in request
	if err := json.Unmarshal(params, &in); err != nil || in.Now < 0 {
		return nil, &bindings.HandlerError{Invalid: true, Message: "invalid JSON input"}
	}
	failed := &bindings.HandlerError{Message: "handler failed"}
	minute := in.Now / 60000
	object, err := bindings.OpenObject("tenants/synthetic/rate/" + in.WorkspaceID)
	if err != nil {
		return nil, failed
	}
	defer object.Close()
	prior, err := object.Read()
	if err != nil {
		return nil, failed
	}
	var current bucket
	if len(prior) > 0 {
		if json.Unmarshal(prior, &current) != nil {
			return nil, failed
		}
	}
	same := len(prior) > 0 && current.Minute == minute
	if same && current.Count >= in.Limit {
		return []byte(rateLimited), nil
	}
	count := int64(1)
	if same {
		count = current.Count + 1
	}
	next, err := json.Marshal(bucket{Minute: minute, Count: count})
	if err != nil || object.Write(next) != nil {
		return nil, failed
	}
	payload, err := json.Marshal(event{
		EventType:   "openapi.spec.accessed",
		Timestamp:   isoTime(in.Now),
		WorkspaceID: in.WorkspaceID,
		TenantID:    in.TenantID,
		SpecVersion: in.SpecVersion,
		RequesterID: in.RequesterID,
		Format:      in.Format,
	})
	if err != nil || bindings.Publish(topic, payload) != nil {
		return nil, failed
	}
	return []byte(accepted), nil
}
