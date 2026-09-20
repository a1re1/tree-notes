export const VERSION = "1.0.0";

export interface Widget {
  render(): string;
}

export type WidgetId = string;

export enum Kind {
  Fast,
  Slow,
}

export function render(id: WidgetId): string {
  return id;
}

export const renderTwice = (id: WidgetId): string => render(id) + render(id);

export class Panel implements Widget {
  static count = 0;
  private title: string = "panel";

  constructor(title: string) {
    this.title = title;
  }

  get heading(): string {
    return this.title;
  }

  render(): string {
    return this.title;
  }
}

export namespace Inner {
  export function deep(): void {}
}
