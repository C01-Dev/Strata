# TS 7 removed baseUrl (was "."): prefix bare path targets with "./" and add the documented
# `"*": ["./*"]` replacement so bare imports like `app/_utils` still resolve from apps/web.
# TS 7 also removed target=es5 and moduleResolution=node10, inherited from @calcom/tsconfig; override them
# in apps/web only, with Next.js's own defaults.
perl -pi -e 's/^\s*"baseUrl": "\.",\n//; s/\["([^.][^"]*)"\]/[".\/$1"]/; s/"compilerOptions": \{/"compilerOptions": {\n    "target": "es2017",\n    "moduleResolution": "bundler",/; s/"paths": \{/"paths": {\n      "*": [".\/*"],/' apps/web/tsconfig.json
