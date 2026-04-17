"""OrderRouter ingestion handler — Python side of the multi-lang fixture."""

from dataclasses import dataclass
from typing import Iterable


@dataclass(frozen=True)
class OrderEvent:
    order_id: str
    customer_id: str
    line_count: int


def normalize(events: Iterable[OrderEvent]) -> list[OrderEvent]:
    """Drop empty orders, then sort by customer for stable downstream batching."""
    return sorted(
        (e for e in events if e.line_count > 0),
        key=lambda e: e.customer_id,
    )


def fan_out(events: Iterable[OrderEvent]) -> dict[str, list[OrderEvent]]:
    by_customer: dict[str, list[OrderEvent]] = {}
    for event in normalize(events):
        by_customer.setdefault(event.customer_id, []).append(event)
    return by_customer
