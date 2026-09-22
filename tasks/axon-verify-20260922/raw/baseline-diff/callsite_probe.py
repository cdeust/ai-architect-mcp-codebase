import json,pathlib,sys
sys.path.insert(0,sys.argv[1])
from measure import Session
out=pathlib.Path(sys.argv[3]); out.mkdir(parents=True,exist_ok=True)
s=Session(sys.argv[2],out); g={'graph_path':sys.argv[4]}
try:
    for k,q in {'cs-count':"MATCH (n:CallSite) RETURN count(n)",
                'bare-sites':"MATCH (n:CallSite) WHERE NOT n.callee_name CONTAINS '.' AND NOT n.callee_name CONTAINS '::' AND n.callee_name IN ['deadline','jitter','blocking','t','meets','bound','is_bounded','push','response_of'] RETURN n.callee_name, count(n) ORDER BY n.callee_name"}.items():
        p=json.loads(s.send('query_graph',{**g,'query':q,'format':'tabular'},k)['result']['content'][0]['text'])
        print(k,p.get('rows'))
finally: s.close()
