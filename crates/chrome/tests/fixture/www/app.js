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
    document.body.appendChild(box);
  }
  window.app = { main, throwCaught, throwLater, spin, loads: () => loads };
  showBox();
  main();
})();
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsiLi4vc3JjL3V0aWwudHMiLCAiLi4vc3JjL2FwcC50cyJdLAogICJzb3VyY2VzQ29udGVudCI6IFsiLy8gSGVscGVycyB0aGUgZml4dHVyZSdzIGFwcCBjYWxscy4gTWFya2VycyAoQG5hbWUpIG5hbWUgdGhlIGxpbmVzIHRoZSB0ZXN0cyB1c2UuXG5cbmV4cG9ydCBmdW5jdGlvbiBhZGQoYTogbnVtYmVyLCBiOiBudW1iZXIpOiBudW1iZXIge1xuXHRjb25zdCBuZXh0ID0gYSArIGI7IC8vIEBhZGRcblx0cmV0dXJuIG5leHQ7IC8vIEBhZGRSZXR1cm5cbn1cblxuZXhwb3J0IGNsYXNzIENvdW50ZXIge1xuXHRjb3VudCA9IDA7XG5cblx0Y29uc3RydWN0b3IocHVibGljIG5hbWU6IHN0cmluZykge31cblxuXHRnZXQgZG91YmxlZCgpOiBudW1iZXIge1xuXHRcdHJldHVybiB0aGlzLmNvdW50ICogMjtcblx0fVxuXG5cdGJ1bXAoKTogdm9pZCB7XG5cdFx0dGhpcy5jb3VudCsrOyAvLyBAYnVtcFxuXHR9XG59XG5cbmV4cG9ydCBmdW5jdGlvbiBmYWlsKG1lc3NhZ2U6IHN0cmluZyk6IG5ldmVyIHtcblx0dGhyb3cgbmV3IEVycm9yKG1lc3NhZ2UpOyAvLyBAdGhyb3dcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIG1ha2VCb3gobGFiZWw6IHN0cmluZyk6IEhUTUxFbGVtZW50IHtcblx0Y29uc3QgYm94ID0gZG9jdW1lbnQuY3JlYXRlRWxlbWVudChcImRpdlwiKTsgLy8gQGNyZWF0ZVxuXHRib3gudGV4dENvbnRlbnQgPSBsYWJlbDtcblx0cmV0dXJuIGJveDtcbn1cbiIsICIvLyBUaGUgcGFnZSB0aGUgdGVzdHMgZGVidWcuIE1hcmtlcnMgKEBuYW1lKSBuYW1lIHRoZSBsaW5lcyB0aGUgdGVzdHMgdXNlLlxuaW1wb3J0IHsgYWRkLCBDb3VudGVyLCBmYWlsLCBtYWtlQm94IH0gZnJvbSBcIi4vdXRpbFwiO1xuXG5pbnRlcmZhY2UgT3JkZXIge1xuXHRpZDogbnVtYmVyO1xuXHRuYW1lOiBzdHJpbmc7XG5cdGl0ZW1zOiBudW1iZXJbXTtcbn1cblxubGV0IGxvYWRzID0gMDtcbmNvbnN0IHRhZ3MgPSBuZXcgTWFwPHN0cmluZywgbnVtYmVyPihbW1wiYVwiLCAxXSwgW1wiYlwiLCAyXV0pO1xuXG5mdW5jdGlvbiB0b3RhbChvcmRlcjogT3JkZXIpOiBudW1iZXIge1xuXHRsZXQgc3VtID0gMDsgLy8gQHN1bVxuXHRmb3IgKGNvbnN0IGl0ZW0gb2Ygb3JkZXIuaXRlbXMpIHtcblx0XHRzdW0gPSBhZGQoc3VtLCBpdGVtKTsgLy8gQGxvb3Bcblx0fVxuXHRyZXR1cm4gc3VtOyAvLyBAcmV0dXJuXG59XG5cbmZ1bmN0aW9uIG1haW4oKTogbnVtYmVyIHtcblx0bG9hZHMrKzsgLy8gQG1haW5cblx0Y29uc3Qgb3JkZXI6IE9yZGVyID0geyBpZDogMywgbmFtZTogXCJBbm5cIiwgaXRlbXM6IFsxLCAyLCAzXSB9O1xuXHRjb25zdCBjb3VudGVyID0gbmV3IENvdW50ZXIoXCJvcmRlcnNcIik7XG5cdGNvdW50ZXIuYnVtcCgpO1xuXHRjb25zdCByZXN1bHQgPSB0b3RhbChvcmRlcik7IC8vIEBjYWxsXG5cdGNvbnNvbGUubG9nKFwidG90YWxcIiwgcmVzdWx0LCBvcmRlci5uYW1lKTsgLy8gQGxvZ1xuXHRyZXR1cm4gcmVzdWx0ICsgdGFncy5zaXplOyAvLyBAYWZ0ZXJcbn1cblxuZnVuY3Rpb24gdGhyb3dDYXVnaHQoKTogc3RyaW5nIHtcblx0dHJ5IHtcblx0XHRmYWlsKFwiY2F1Z2h0IG9uZVwiKTtcblx0fSBjYXRjaCAoZXJyKSB7XG5cdFx0cmV0dXJuIFN0cmluZyhlcnIpO1xuXHR9XG59XG5cbmZ1bmN0aW9uIHRocm93TGF0ZXIoKTogdm9pZCB7XG5cdHNldFRpbWVvdXQoKCkgPT4gZmFpbChcInVuY2F1Z2h0IG9uZVwiKSwgMCk7XG59XG5cbmZ1bmN0aW9uIHNwaW4objogbnVtYmVyKTogbnVtYmVyIHtcblx0bGV0IGFjYyA9IDA7XG5cdGZvciAobGV0IGkgPSAwOyBpIDwgbjsgaSsrKSB7XG5cdFx0YWNjICs9IGk7IC8vIEBzcGluXG5cdH1cblx0cmV0dXJuIGFjYztcbn1cblxuZnVuY3Rpb24gc2hvd0JveCgpOiB2b2lkIHtcblx0Y29uc3QgYm94ID0gbWFrZUJveChcImEgYm94XCIpOyAvLyBAYm94XG5cdGJveC5pZCA9IFwiYm94XCI7XG5cdGJveC5zdHlsZS5jc3NUZXh0ID0gXCJ3aWR0aDogMjAwcHg7IGhlaWdodDogMTAwcHhcIjsgLy8gQHN0eWxlXG5cdGRvY3VtZW50LmJvZHkuYXBwZW5kQ2hpbGQoYm94KTtcbn1cblxuKHdpbmRvdyBhcyBhbnkpLmFwcCA9IHsgbWFpbiwgdGhyb3dDYXVnaHQsIHRocm93TGF0ZXIsIHNwaW4sIGxvYWRzOiAoKSA9PiBsb2FkcyB9O1xuc2hvd0JveCgpO1xubWFpbigpO1xuIl0sCiAgIm1hcHBpbmdzIjogIjs7Ozs7O0FBRU8sV0FBUyxJQUFJLEdBQVcsR0FBbUI7QUFDakQsVUFBTSxPQUFPLElBQUk7QUFDakIsV0FBTztBQUFBLEVBQ1I7QUFFTyxNQUFNLFVBQU4sTUFBYztBQUFBLElBR3BCLFlBQW1CLE1BQWM7QUFBZDtBQUZuQixtQ0FBUTtBQUFBLElBRTBCO0FBQUEsSUFFbEMsSUFBSSxVQUFrQjtBQUNyQixhQUFPLEtBQUssUUFBUTtBQUFBLElBQ3JCO0FBQUEsSUFFQSxPQUFhO0FBQ1osV0FBSztBQUFBLElBQ047QUFBQSxFQUNEO0FBRU8sV0FBUyxLQUFLLFNBQXdCO0FBQzVDLFVBQU0sSUFBSSxNQUFNLE9BQU87QUFBQSxFQUN4QjtBQUVPLFdBQVMsUUFBUSxPQUE0QjtBQUNuRCxVQUFNLE1BQU0sU0FBUyxjQUFjLEtBQUs7QUFDeEMsUUFBSSxjQUFjO0FBQ2xCLFdBQU87QUFBQSxFQUNSOzs7QUNwQkEsTUFBSSxRQUFRO0FBQ1osTUFBTSxPQUFPLG9CQUFJLElBQW9CLENBQUMsQ0FBQyxLQUFLLENBQUMsR0FBRyxDQUFDLEtBQUssQ0FBQyxDQUFDLENBQUM7QUFFekQsV0FBUyxNQUFNLE9BQXNCO0FBQ3BDLFFBQUksTUFBTTtBQUNWLGVBQVcsUUFBUSxNQUFNLE9BQU87QUFDL0IsWUFBTSxJQUFJLEtBQUssSUFBSTtBQUFBLElBQ3BCO0FBQ0EsV0FBTztBQUFBLEVBQ1I7QUFFQSxXQUFTLE9BQWU7QUFDdkI7QUFDQSxVQUFNLFFBQWUsRUFBRSxJQUFJLEdBQUcsTUFBTSxPQUFPLE9BQU8sQ0FBQyxHQUFHLEdBQUcsQ0FBQyxFQUFFO0FBQzVELFVBQU0sVUFBVSxJQUFJLFFBQVEsUUFBUTtBQUNwQyxZQUFRLEtBQUs7QUFDYixVQUFNLFNBQVMsTUFBTSxLQUFLO0FBQzFCLFlBQVEsSUFBSSxTQUFTLFFBQVEsTUFBTSxJQUFJO0FBQ3ZDLFdBQU8sU0FBUyxLQUFLO0FBQUEsRUFDdEI7QUFFQSxXQUFTLGNBQXNCO0FBQzlCLFFBQUk7QUFDSCxXQUFLLFlBQVk7QUFBQSxJQUNsQixTQUFTLEtBQUs7QUFDYixhQUFPLE9BQU8sR0FBRztBQUFBLElBQ2xCO0FBQUEsRUFDRDtBQUVBLFdBQVMsYUFBbUI7QUFDM0IsZUFBVyxNQUFNLEtBQUssY0FBYyxHQUFHLENBQUM7QUFBQSxFQUN6QztBQUVBLFdBQVMsS0FBSyxHQUFtQjtBQUNoQyxRQUFJLE1BQU07QUFDVixhQUFTLElBQUksR0FBRyxJQUFJLEdBQUcsS0FBSztBQUMzQixhQUFPO0FBQUEsSUFDUjtBQUNBLFdBQU87QUFBQSxFQUNSO0FBRUEsV0FBUyxVQUFnQjtBQUN4QixVQUFNLE1BQU0sUUFBUSxPQUFPO0FBQzNCLFFBQUksS0FBSztBQUNULFFBQUksTUFBTSxVQUFVO0FBQ3BCLGFBQVMsS0FBSyxZQUFZLEdBQUc7QUFBQSxFQUM5QjtBQUVBLEVBQUMsT0FBZSxNQUFNLEVBQUUsTUFBTSxhQUFhLFlBQVksTUFBTSxPQUFPLE1BQU0sTUFBTTtBQUNoRixVQUFRO0FBQ1IsT0FBSzsiLAogICJuYW1lcyI6IFtdCn0K
