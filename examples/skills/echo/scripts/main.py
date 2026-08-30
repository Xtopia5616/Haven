import sys, json
params = json.load(sys.stdin)
print(json.dumps({"echo": params}))
