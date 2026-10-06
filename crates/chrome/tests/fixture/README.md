The page the `chrome` crate's tests debug. `www/app.js` (inline source map) and
`www/linked/app.js` (with `app.js.map` next to it) were made with esbuild 0.28.1
from this folder:

    esbuild src/app.ts --bundle --format=iife --target=es2020 --sourcemap=inline --outfile=www/app.js
    esbuild src/app.ts --bundle --format=iife --target=es2020 --sourcemap --outfile=www/linked/app.js

Run both again after changing `src/`. The tests find lines by their `// @name` markers.
