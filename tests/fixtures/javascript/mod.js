export const LIMIT = 10;

export function load(name) {
  return name;
}

export const fetchAll = (names) => names.map(load);

export const make = function (name) {
  return name;
};

export class Queue {
  #items = [];

  constructor() {
    this.#items = [];
  }

  push(item) {
    this.#items.push(item);
  }
}
