// The page the tests debug. Markers (@name) name the lines the tests use.
import { add, Counter, fail } from "./util";

interface Order {
	id: number;
	name: string;
	items: number[];
}

let loads = 0;
const tags = new Map<string, number>([["a", 1], ["b", 2]]);

function total(order: Order): number {
	let sum = 0; // @sum
	for (const item of order.items) {
		sum = add(sum, item); // @loop
	}
	return sum; // @return
}

function main(): number {
	loads++; // @main
	const order: Order = { id: 3, name: "Ann", items: [1, 2, 3] };
	const counter = new Counter("orders");
	counter.bump();
	const result = total(order); // @call
	console.log("total", result, order.name); // @log
	return result + tags.size; // @after
}

function throwCaught(): string {
	try {
		fail("caught one");
	} catch (err) {
		return String(err);
	}
}

function throwLater(): void {
	setTimeout(() => fail("uncaught one"), 0);
}

function spin(n: number): number {
	let acc = 0;
	for (let i = 0; i < n; i++) {
		acc += i; // @spin
	}
	return acc;
}

(window as any).app = { main, throwCaught, throwLater, spin, loads: () => loads };
main();
