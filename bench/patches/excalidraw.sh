# TS 7 removed baseUrl; paths are already "./"-relative to the tsconfig dir.
sed -i '' '/"baseUrl": "."/d' tsconfig.json
