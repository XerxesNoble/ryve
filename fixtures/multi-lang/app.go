// Package orderrouter is the Go side of the multi-lang fixture: a fan-out
// scheduler that groups OrderEvents by customer and dispatches each batch
// downstream.
package orderrouter

import "sort"

type OrderEvent struct {
	OrderID    string
	CustomerID string
	LineCount  int
}

// FanOut groups events by CustomerID and returns the customer ids in
// deterministic order so downstream consumers see a stable batching plan.
func FanOut(events []OrderEvent) ([]string, map[string][]OrderEvent) {
	groups := make(map[string][]OrderEvent)
	for _, e := range events {
		if e.LineCount <= 0 {
			continue
		}
		groups[e.CustomerID] = append(groups[e.CustomerID], e)
	}
	customers := make([]string, 0, len(groups))
	for c := range groups {
		customers = append(customers, c)
	}
	sort.Strings(customers)
	return customers, groups
}
