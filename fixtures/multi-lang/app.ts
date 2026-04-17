// OrderRouter validation worker — TypeScript side of the multi-lang fixture.

export interface OrderEvent {
  readonly orderId: string;
  readonly customerId: string;
  readonly lineCount: number;
}

export class ValidationError extends Error {
  constructor(message: string, readonly orderId: string) {
    super(message);
    this.name = "ValidationError";
  }
}

export function validate(event: OrderEvent): OrderEvent {
  if (!event.orderId) {
    throw new ValidationError("missing orderId", event.orderId);
  }
  if (event.lineCount <= 0) {
    throw new ValidationError("lineCount must be positive", event.orderId);
  }
  return event;
}

export function validateAll(events: readonly OrderEvent[]): OrderEvent[] {
  return events.map(validate);
}
