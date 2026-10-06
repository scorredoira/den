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
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsiLi4vc3JjL3V0aWwudHMiLCAiLi4vc3JjL2FwcC50cyJdLAogICJzb3VyY2VzQ29udGVudCI6IFsiLy8gSGVscGVycyB0aGUgZml4dHVyZSdzIGFwcCBjYWxscy4gTWFya2VycyAoQG5hbWUpIG5hbWUgdGhlIGxpbmVzIHRoZSB0ZXN0cyB1c2UuXG5cbmV4cG9ydCBmdW5jdGlvbiBhZGQoYTogbnVtYmVyLCBiOiBudW1iZXIpOiBudW1iZXIge1xuXHRjb25zdCBuZXh0ID0gYSArIGI7IC8vIEBhZGRcblx0cmV0dXJuIG5leHQ7IC8vIEBhZGRSZXR1cm5cbn1cblxuZXhwb3J0IGNsYXNzIENvdW50ZXIge1xuXHRjb3VudCA9IDA7XG5cblx0Y29uc3RydWN0b3IocHVibGljIG5hbWU6IHN0cmluZykge31cblxuXHRidW1wKCk6IHZvaWQge1xuXHRcdHRoaXMuY291bnQrKzsgLy8gQGJ1bXBcblx0fVxufVxuXG5leHBvcnQgZnVuY3Rpb24gZmFpbChtZXNzYWdlOiBzdHJpbmcpOiBuZXZlciB7XG5cdHRocm93IG5ldyBFcnJvcihtZXNzYWdlKTsgLy8gQHRocm93XG59XG4iLCAiLy8gVGhlIHBhZ2UgdGhlIHRlc3RzIGRlYnVnLiBNYXJrZXJzIChAbmFtZSkgbmFtZSB0aGUgbGluZXMgdGhlIHRlc3RzIHVzZS5cbmltcG9ydCB7IGFkZCwgQ291bnRlciwgZmFpbCB9IGZyb20gXCIuL3V0aWxcIjtcblxuaW50ZXJmYWNlIE9yZGVyIHtcblx0aWQ6IG51bWJlcjtcblx0bmFtZTogc3RyaW5nO1xuXHRpdGVtczogbnVtYmVyW107XG59XG5cbmxldCBsb2FkcyA9IDA7XG5jb25zdCB0YWdzID0gbmV3IE1hcDxzdHJpbmcsIG51bWJlcj4oW1tcImFcIiwgMV0sIFtcImJcIiwgMl1dKTtcblxuZnVuY3Rpb24gdG90YWwob3JkZXI6IE9yZGVyKTogbnVtYmVyIHtcblx0bGV0IHN1bSA9IDA7IC8vIEBzdW1cblx0Zm9yIChjb25zdCBpdGVtIG9mIG9yZGVyLml0ZW1zKSB7XG5cdFx0c3VtID0gYWRkKHN1bSwgaXRlbSk7IC8vIEBsb29wXG5cdH1cblx0cmV0dXJuIHN1bTsgLy8gQHJldHVyblxufVxuXG5mdW5jdGlvbiBtYWluKCk6IG51bWJlciB7XG5cdGxvYWRzKys7IC8vIEBtYWluXG5cdGNvbnN0IG9yZGVyOiBPcmRlciA9IHsgaWQ6IDMsIG5hbWU6IFwiQW5uXCIsIGl0ZW1zOiBbMSwgMiwgM10gfTtcblx0Y29uc3QgY291bnRlciA9IG5ldyBDb3VudGVyKFwib3JkZXJzXCIpO1xuXHRjb3VudGVyLmJ1bXAoKTtcblx0Y29uc3QgcmVzdWx0ID0gdG90YWwob3JkZXIpOyAvLyBAY2FsbFxuXHRjb25zb2xlLmxvZyhcInRvdGFsXCIsIHJlc3VsdCwgb3JkZXIubmFtZSk7IC8vIEBsb2dcblx0cmV0dXJuIHJlc3VsdCArIHRhZ3Muc2l6ZTsgLy8gQGFmdGVyXG59XG5cbmZ1bmN0aW9uIHRocm93Q2F1Z2h0KCk6IHN0cmluZyB7XG5cdHRyeSB7XG5cdFx0ZmFpbChcImNhdWdodCBvbmVcIik7XG5cdH0gY2F0Y2ggKGVycikge1xuXHRcdHJldHVybiBTdHJpbmcoZXJyKTtcblx0fVxufVxuXG5mdW5jdGlvbiB0aHJvd0xhdGVyKCk6IHZvaWQge1xuXHRzZXRUaW1lb3V0KCgpID0+IGZhaWwoXCJ1bmNhdWdodCBvbmVcIiksIDApO1xufVxuXG5mdW5jdGlvbiBzcGluKG46IG51bWJlcik6IG51bWJlciB7XG5cdGxldCBhY2MgPSAwO1xuXHRmb3IgKGxldCBpID0gMDsgaSA8IG47IGkrKykge1xuXHRcdGFjYyArPSBpOyAvLyBAc3BpblxuXHR9XG5cdHJldHVybiBhY2M7XG59XG5cbih3aW5kb3cgYXMgYW55KS5hcHAgPSB7IG1haW4sIHRocm93Q2F1Z2h0LCB0aHJvd0xhdGVyLCBzcGluLCBsb2FkczogKCkgPT4gbG9hZHMgfTtcbm1haW4oKTtcbiJdLAogICJtYXBwaW5ncyI6ICI7Ozs7OztBQUVPLFdBQVMsSUFBSSxHQUFXLEdBQW1CO0FBQ2pELFVBQU0sT0FBTyxJQUFJO0FBQ2pCLFdBQU87QUFBQSxFQUNSO0FBRU8sTUFBTSxVQUFOLE1BQWM7QUFBQSxJQUdwQixZQUFtQixNQUFjO0FBQWQ7QUFGbkIsbUNBQVE7QUFBQSxJQUUwQjtBQUFBLElBRWxDLE9BQWE7QUFDWixXQUFLO0FBQUEsSUFDTjtBQUFBLEVBQ0Q7QUFFTyxXQUFTLEtBQUssU0FBd0I7QUFDNUMsVUFBTSxJQUFJLE1BQU0sT0FBTztBQUFBLEVBQ3hCOzs7QUNWQSxNQUFJLFFBQVE7QUFDWixNQUFNLE9BQU8sb0JBQUksSUFBb0IsQ0FBQyxDQUFDLEtBQUssQ0FBQyxHQUFHLENBQUMsS0FBSyxDQUFDLENBQUMsQ0FBQztBQUV6RCxXQUFTLE1BQU0sT0FBc0I7QUFDcEMsUUFBSSxNQUFNO0FBQ1YsZUFBVyxRQUFRLE1BQU0sT0FBTztBQUMvQixZQUFNLElBQUksS0FBSyxJQUFJO0FBQUEsSUFDcEI7QUFDQSxXQUFPO0FBQUEsRUFDUjtBQUVBLFdBQVMsT0FBZTtBQUN2QjtBQUNBLFVBQU0sUUFBZSxFQUFFLElBQUksR0FBRyxNQUFNLE9BQU8sT0FBTyxDQUFDLEdBQUcsR0FBRyxDQUFDLEVBQUU7QUFDNUQsVUFBTSxVQUFVLElBQUksUUFBUSxRQUFRO0FBQ3BDLFlBQVEsS0FBSztBQUNiLFVBQU0sU0FBUyxNQUFNLEtBQUs7QUFDMUIsWUFBUSxJQUFJLFNBQVMsUUFBUSxNQUFNLElBQUk7QUFDdkMsV0FBTyxTQUFTLEtBQUs7QUFBQSxFQUN0QjtBQUVBLFdBQVMsY0FBc0I7QUFDOUIsUUFBSTtBQUNILFdBQUssWUFBWTtBQUFBLElBQ2xCLFNBQVMsS0FBSztBQUNiLGFBQU8sT0FBTyxHQUFHO0FBQUEsSUFDbEI7QUFBQSxFQUNEO0FBRUEsV0FBUyxhQUFtQjtBQUMzQixlQUFXLE1BQU0sS0FBSyxjQUFjLEdBQUcsQ0FBQztBQUFBLEVBQ3pDO0FBRUEsV0FBUyxLQUFLLEdBQW1CO0FBQ2hDLFFBQUksTUFBTTtBQUNWLGFBQVMsSUFBSSxHQUFHLElBQUksR0FBRyxLQUFLO0FBQzNCLGFBQU87QUFBQSxJQUNSO0FBQ0EsV0FBTztBQUFBLEVBQ1I7QUFFQSxFQUFDLE9BQWUsTUFBTSxFQUFFLE1BQU0sYUFBYSxZQUFZLE1BQU0sT0FBTyxNQUFNLE1BQU07QUFDaEYsT0FBSzsiLAogICJuYW1lcyI6IFtdCn0K
