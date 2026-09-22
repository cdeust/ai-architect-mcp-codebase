import json,pathlib,sys
sys.path.insert(0,sys.argv[1])
from measure import Session
out=pathlib.Path(sys.argv[3]); out.mkdir(parents=True,exist_ok=True)
s=Session(sys.argv[2],out)
g={'graph_path':sys.argv[4]}
qs={'gen-sites':"MATCH (n:CallSite) WHERE n.id STARTS WITH 'tests/properties.rs' AND (n.callee_name CONTAINS 'jitter' OR n.callee_name CONTAINS 'deadline' OR n.callee_name CONTAINS 'blocking' OR n.callee_name CONTAINS 'Task::new') RETURN n.id, n.callee_name, n.line, n.is_resolved ORDER BY n.id",
    'gen-edges':"MATCH (a)-[r]->(b) WHERE a.qualified_name = 'tests/properties.rs::generate' RETURN label(r), b.qualified_name, r.resolution_method ORDER BY b.qualified_name",
    't-edges':"MATCH (a)-[r]->(b) WHERE b.qualified_name = 'src/lib.rs::tests::t' RETURN a.qualified_name, label(r), r.resolution_method ORDER BY a.qualified_name"}
try:
    for k,q in qs.items():
        rep=s.send('query_graph',{**g,'query':q,'format':'tabular','limit':200},k)
        p=json.loads(rep['result']['content'][0]['text'])
        print('==',k,p.get('status'),p.get('message','')[:200]); 
        for r in p.get('rows',[]): print('   ',r)
finally: s.close()
