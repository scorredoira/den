// Helpers the fixture's app calls. Markers (@name) name the lines the tests use.

export function add(a: number, b: number): number {
	const next = a + b; // @add
	return next; // @addReturn
}

export class Counter {
	count = 0;

	constructor(public name: string) {}

	get doubled(): number {
		return this.count * 2;
	}

	bump(): void {
		this.count++; // @bump
	}
}

export function fail(message: string): never {
	throw new Error(message); // @throw
}

export function makeBox(label: string): HTMLElement {
	const box = document.createElement("div"); // @create
	box.textContent = label;
	return box;
}
