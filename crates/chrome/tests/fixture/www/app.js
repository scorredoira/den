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
    get doubled() {
      return this.count * 2;
    }
    bump() {
      this.count++;
    }
  };
  function fail(message) {
    throw new Error(message);
  }
  function makeBox(label) {
    const box = document.createElement("div");
    box.textContent = label;
    return box;
  }

  // src/app.ts
  var loads = 0;
  var boxClicks = 0;
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
  function showBox() {
    const box = makeBox("a box");
    box.id = "box";
    box.style.cssText = "width: 200px; height: 100px";
    box.addEventListener("click", () => {
      boxClicks++;
    });
    document.body.appendChild(box);
  }
  window.app = { main, throwCaught, throwLater, spin, loads: () => loads, boxClicks: () => boxClicks };
  showBox();
  main();
})();
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsiLi4vc3JjL3V0aWwudHMiLCAiLi4vc3JjL2FwcC50cyJdLAogICJzb3VyY2VzQ29udGVudCI6IFsiLy8gSGVscGVycyB0aGUgZml4dHVyZSdzIGFwcCBjYWxscy4gTWFya2VycyAoQG5hbWUpIG5hbWUgdGhlIGxpbmVzIHRoZSB0ZXN0cyB1c2UuXG5cbmV4cG9ydCBmdW5jdGlvbiBhZGQoYTogbnVtYmVyLCBiOiBudW1iZXIpOiBudW1iZXIge1xuXHRjb25zdCBuZXh0ID0gYSArIGI7IC8vIEBhZGRcblx0cmV0dXJuIG5leHQ7IC8vIEBhZGRSZXR1cm5cbn1cblxuZXhwb3J0IGNsYXNzIENvdW50ZXIge1xuXHRjb3VudCA9IDA7XG5cblx0Y29uc3RydWN0b3IocHVibGljIG5hbWU6IHN0cmluZykge31cblxuXHRnZXQgZG91YmxlZCgpOiBudW1iZXIge1xuXHRcdHJldHVybiB0aGlzLmNvdW50ICogMjtcblx0fVxuXG5cdGJ1bXAoKTogdm9pZCB7XG5cdFx0dGhpcy5jb3VudCsrOyAvLyBAYnVtcFxuXHR9XG59XG5cbmV4cG9ydCBmdW5jdGlvbiBmYWlsKG1lc3NhZ2U6IHN0cmluZyk6IG5ldmVyIHtcblx0dGhyb3cgbmV3IEVycm9yKG1lc3NhZ2UpOyAvLyBAdGhyb3dcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIG1ha2VCb3gobGFiZWw6IHN0cmluZyk6IEhUTUxFbGVtZW50IHtcblx0Y29uc3QgYm94ID0gZG9jdW1lbnQuY3JlYXRlRWxlbWVudChcImRpdlwiKTsgLy8gQGNyZWF0ZVxuXHRib3gudGV4dENvbnRlbnQgPSBsYWJlbDtcblx0cmV0dXJuIGJveDtcbn1cbiIsICIvLyBUaGUgcGFnZSB0aGUgdGVzdHMgZGVidWcuIE1hcmtlcnMgKEBuYW1lKSBuYW1lIHRoZSBsaW5lcyB0aGUgdGVzdHMgdXNlLlxuaW1wb3J0IHsgYWRkLCBDb3VudGVyLCBmYWlsLCBtYWtlQm94IH0gZnJvbSBcIi4vdXRpbFwiO1xuXG5pbnRlcmZhY2UgT3JkZXIge1xuXHRpZDogbnVtYmVyO1xuXHRuYW1lOiBzdHJpbmc7XG5cdGl0ZW1zOiBudW1iZXJbXTtcbn1cblxubGV0IGxvYWRzID0gMDtcbmxldCBib3hDbGlja3MgPSAwO1xuY29uc3QgdGFncyA9IG5ldyBNYXA8c3RyaW5nLCBudW1iZXI+KFtbXCJhXCIsIDFdLCBbXCJiXCIsIDJdXSk7XG5cbmZ1bmN0aW9uIHRvdGFsKG9yZGVyOiBPcmRlcik6IG51bWJlciB7XG5cdGxldCBzdW0gPSAwOyAvLyBAc3VtXG5cdGZvciAoY29uc3QgaXRlbSBvZiBvcmRlci5pdGVtcykge1xuXHRcdHN1bSA9IGFkZChzdW0sIGl0ZW0pOyAvLyBAbG9vcFxuXHR9XG5cdHJldHVybiBzdW07IC8vIEByZXR1cm5cbn1cblxuZnVuY3Rpb24gbWFpbigpOiBudW1iZXIge1xuXHRsb2FkcysrOyAvLyBAbWFpblxuXHRjb25zdCBvcmRlcjogT3JkZXIgPSB7IGlkOiAzLCBuYW1lOiBcIkFublwiLCBpdGVtczogWzEsIDIsIDNdIH07XG5cdGNvbnN0IGNvdW50ZXIgPSBuZXcgQ291bnRlcihcIm9yZGVyc1wiKTtcblx0Y291bnRlci5idW1wKCk7XG5cdGNvbnN0IHJlc3VsdCA9IHRvdGFsKG9yZGVyKTsgLy8gQGNhbGxcblx0Y29uc29sZS5sb2coXCJ0b3RhbFwiLCByZXN1bHQsIG9yZGVyLm5hbWUpOyAvLyBAbG9nXG5cdHJldHVybiByZXN1bHQgKyB0YWdzLnNpemU7IC8vIEBhZnRlclxufVxuXG5mdW5jdGlvbiB0aHJvd0NhdWdodCgpOiBzdHJpbmcge1xuXHR0cnkge1xuXHRcdGZhaWwoXCJjYXVnaHQgb25lXCIpO1xuXHR9IGNhdGNoIChlcnIpIHtcblx0XHRyZXR1cm4gU3RyaW5nKGVycik7XG5cdH1cbn1cblxuZnVuY3Rpb24gdGhyb3dMYXRlcigpOiB2b2lkIHtcblx0c2V0VGltZW91dCgoKSA9PiBmYWlsKFwidW5jYXVnaHQgb25lXCIpLCAwKTtcbn1cblxuZnVuY3Rpb24gc3BpbihuOiBudW1iZXIpOiBudW1iZXIge1xuXHRsZXQgYWNjID0gMDtcblx0Zm9yIChsZXQgaSA9IDA7IGkgPCBuOyBpKyspIHtcblx0XHRhY2MgKz0gaTsgLy8gQHNwaW5cblx0fVxuXHRyZXR1cm4gYWNjO1xufVxuXG5mdW5jdGlvbiBzaG93Qm94KCk6IHZvaWQge1xuXHRjb25zdCBib3ggPSBtYWtlQm94KFwiYSBib3hcIik7IC8vIEBib3hcblx0Ym94LmlkID0gXCJib3hcIjtcblx0Ym94LnN0eWxlLmNzc1RleHQgPSBcIndpZHRoOiAyMDBweDsgaGVpZ2h0OiAxMDBweFwiOyAvLyBAc3R5bGVcblx0Ym94LmFkZEV2ZW50TGlzdGVuZXIoXCJjbGlja1wiLCAoKSA9PiB7XG5cdFx0Ym94Q2xpY2tzKys7XG5cdH0pO1xuXHRkb2N1bWVudC5ib2R5LmFwcGVuZENoaWxkKGJveCk7XG59XG5cbih3aW5kb3cgYXMgYW55KS5hcHAgPSB7IG1haW4sIHRocm93Q2F1Z2h0LCB0aHJvd0xhdGVyLCBzcGluLCBsb2FkczogKCkgPT4gbG9hZHMsIGJveENsaWNrczogKCkgPT4gYm94Q2xpY2tzIH07XG5zaG93Qm94KCk7XG5tYWluKCk7XG4iXSwKICAibWFwcGluZ3MiOiAiOzs7Ozs7QUFFTyxXQUFTLElBQUksR0FBVyxHQUFtQjtBQUNqRCxVQUFNLE9BQU8sSUFBSTtBQUNqQixXQUFPO0FBQUEsRUFDUjtBQUVPLE1BQU0sVUFBTixNQUFjO0FBQUEsSUFHcEIsWUFBbUIsTUFBYztBQUFkO0FBRm5CLG1DQUFRO0FBQUEsSUFFMEI7QUFBQSxJQUVsQyxJQUFJLFVBQWtCO0FBQ3JCLGFBQU8sS0FBSyxRQUFRO0FBQUEsSUFDckI7QUFBQSxJQUVBLE9BQWE7QUFDWixXQUFLO0FBQUEsSUFDTjtBQUFBLEVBQ0Q7QUFFTyxXQUFTLEtBQUssU0FBd0I7QUFDNUMsVUFBTSxJQUFJLE1BQU0sT0FBTztBQUFBLEVBQ3hCO0FBRU8sV0FBUyxRQUFRLE9BQTRCO0FBQ25ELFVBQU0sTUFBTSxTQUFTLGNBQWMsS0FBSztBQUN4QyxRQUFJLGNBQWM7QUFDbEIsV0FBTztBQUFBLEVBQ1I7OztBQ3BCQSxNQUFJLFFBQVE7QUFDWixNQUFJLFlBQVk7QUFDaEIsTUFBTSxPQUFPLG9CQUFJLElBQW9CLENBQUMsQ0FBQyxLQUFLLENBQUMsR0FBRyxDQUFDLEtBQUssQ0FBQyxDQUFDLENBQUM7QUFFekQsV0FBUyxNQUFNLE9BQXNCO0FBQ3BDLFFBQUksTUFBTTtBQUNWLGVBQVcsUUFBUSxNQUFNLE9BQU87QUFDL0IsWUFBTSxJQUFJLEtBQUssSUFBSTtBQUFBLElBQ3BCO0FBQ0EsV0FBTztBQUFBLEVBQ1I7QUFFQSxXQUFTLE9BQWU7QUFDdkI7QUFDQSxVQUFNLFFBQWUsRUFBRSxJQUFJLEdBQUcsTUFBTSxPQUFPLE9BQU8sQ0FBQyxHQUFHLEdBQUcsQ0FBQyxFQUFFO0FBQzVELFVBQU0sVUFBVSxJQUFJLFFBQVEsUUFBUTtBQUNwQyxZQUFRLEtBQUs7QUFDYixVQUFNLFNBQVMsTUFBTSxLQUFLO0FBQzFCLFlBQVEsSUFBSSxTQUFTLFFBQVEsTUFBTSxJQUFJO0FBQ3ZDLFdBQU8sU0FBUyxLQUFLO0FBQUEsRUFDdEI7QUFFQSxXQUFTLGNBQXNCO0FBQzlCLFFBQUk7QUFDSCxXQUFLLFlBQVk7QUFBQSxJQUNsQixTQUFTLEtBQUs7QUFDYixhQUFPLE9BQU8sR0FBRztBQUFBLElBQ2xCO0FBQUEsRUFDRDtBQUVBLFdBQVMsYUFBbUI7QUFDM0IsZUFBVyxNQUFNLEtBQUssY0FBYyxHQUFHLENBQUM7QUFBQSxFQUN6QztBQUVBLFdBQVMsS0FBSyxHQUFtQjtBQUNoQyxRQUFJLE1BQU07QUFDVixhQUFTLElBQUksR0FBRyxJQUFJLEdBQUcsS0FBSztBQUMzQixhQUFPO0FBQUEsSUFDUjtBQUNBLFdBQU87QUFBQSxFQUNSO0FBRUEsV0FBUyxVQUFnQjtBQUN4QixVQUFNLE1BQU0sUUFBUSxPQUFPO0FBQzNCLFFBQUksS0FBSztBQUNULFFBQUksTUFBTSxVQUFVO0FBQ3BCLFFBQUksaUJBQWlCLFNBQVMsTUFBTTtBQUNuQztBQUFBLElBQ0QsQ0FBQztBQUNELGFBQVMsS0FBSyxZQUFZLEdBQUc7QUFBQSxFQUM5QjtBQUVBLEVBQUMsT0FBZSxNQUFNLEVBQUUsTUFBTSxhQUFhLFlBQVksTUFBTSxPQUFPLE1BQU0sT0FBTyxXQUFXLE1BQU0sVUFBVTtBQUM1RyxVQUFRO0FBQ1IsT0FBSzsiLAogICJuYW1lcyI6IFtdCn0K
