(() => {
  var __defProp = Object.defineProperty;
  var __defNormalProp = (obj, key, value) => key in obj ? __defProp(obj, key, { enumerable: true, configurable: true, writable: true, value }) : obj[key] = value;
  var __publicField = (obj, key, value) => __defNormalProp(obj, typeof key !== "symbol" ? key + "" : key, value);

  // src/util.ts
  function add(a, b) {
    const next = a + b;
    return next;
  }
  var Counter = class {
    constructor(name) {
      __publicField(this, "name", name);
      __publicField(this, "count", 0);
    }
    bump() {
      this.count++;
    }
  };
  function fail(message) {
    throw new Error(message);
  }

  // src/app.ts
  var loads = 0;
  var tags = /* @__PURE__ */ new Map([["a", 1], ["b", 2]]);
  function total(order) {
    let sum = 0;
    for (const item of order.items) {
      sum = add(sum, item);
    }
    return sum;
  }
  function main() {
    loads++;
    const order = { id: 3, name: "Ann", items: [1, 2, 3] };
    const counter = new Counter("orders");
    counter.bump();
    const result = total(order);
    console.log("total", result, order.name);
    return result + tags.size;
  }
  function throwCaught() {
    try {
      fail("caught one");
    } catch (err) {
      return String(err);
    }
  }
  function throwLater() {
    setTimeout(() => fail("uncaught one"), 0);
  }
  function spin(n) {
    let acc = 0;
    for (let i = 0; i < n; i++) {
      acc += i;
    }
    return acc;
  }
  window.app = { main, throwCaught, throwLater, spin, loads: () => loads };
  main();
})();
//# sourceMappingURL=app.js.map
