#!/usr/bin/env python3
"""One-process, fresh-database MCP experiment. Timings are wall-time samples.

Adapted from tasks/axon-rerun-20260908/measure.py. Three additions, nothing
removed and nothing re-ordered, so the `semantic_sha256` computed by score.py
stays comparable with the 2026-09-08 pass:

--analyze-path: analyse a DIFFERENT root than the oracle-checked corpus
(probe E of plan §0.2: analyse the parent workspace root while still gating
corpus integrity on the child's bytes).

--lsp-tool: after analyze_codebase, also drive the standalone `lsp_resolve`
tool against the produced graph, recording its envelope even when it answers
status="error" (issue #282 wants that error, so it must not abort).

extras(): extra read-only query_graph calls appended AFTER every measurement
score.py hashes: CallSite.unresolved_reason (#284) and the resolution_method
of the `response_of` in-edges (#283).

Protocol/schema source: verifier src/main.rs and archived raw/tools-list.json.
Run once for static, once per LSP replicate; output directory must not exist.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import time


def unpack(reply):
    if 'error' in reply:
        raise RuntimeError(reply['error'])
    result = reply['result']
    if result.get('isError'):
        raise RuntimeError(result)
    for block in result.get('content', []):
        if block.get('type') == 'text':
            payload = json.loads(block['text'])
            if payload.get('status') == 'error':
                raise RuntimeError(payload)
            return payload
    return result


class Session:
    def __init__(self, binary, directory):
        self.directory = directory
        self.serial = 0
        self.log = (directory / 'stderr.log').open('w')
        self.process = subprocess.Popen(
            [binary, '--profile', 'full'], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.log, text=True)

    def send(self, name, arguments, tag):
        """Record the request/response pair; return the raw reply unchecked."""
        self.serial += 1
        request = {'jsonrpc': '2.0', 'id': self.serial, 'method': 'tools/call',
                   'params': {'name': name, 'arguments': arguments}}
        start = time.perf_counter()
        self.process.stdin.write(json.dumps(request) + '\n')
        self.process.stdin.flush()
        reply = json.loads(self.process.stdout.readline())
        elapsed = time.perf_counter() - start
        record = {'request': request, 'response': reply, 'wall_seconds': elapsed}
        (self.directory / (tag + '.json')).write_text(json.dumps(record, indent=2))
        print(f'{tag}: {elapsed:.6f}s', flush=True)
        return reply

    def call(self, name, arguments, tag):
        return unpack(self.send(name, arguments, tag))

    def pages(self, name, arguments, tag):
        pages, offset = [], 0
        while True:
            page = self.call(name, {**arguments, 'offset': offset}, f'{tag}-{offset}')
            pages.append(page)
            following = page.get('next_offset')
            if following is None:
                break
            if following <= offset:
                raise RuntimeError('Pagination did not advance')
            offset = following
        (self.directory / (tag + '-pages.json')).write_text(json.dumps(pages, indent=2))

    def close(self):
        self.process.stdin.close()
        self.process.wait()
        self.log.close()


def measurements(session, graph):
    base = {'graph_path': graph}
    session.call('index_status', base, 'status')
    session.call('query_graph', {**base, 'graph': 'missed'}, 'coverage')
    session.pages('get_processes', base, 'processes')
    target = 'src/lib.rs::TaskSet::response_of'
    session.pages('get_impact', {**base, 'qualified_name': target}, 'impact')
    for label in ('Function', 'Method'):
        query = (f'MATCH (n:{label}) RETURN n.qualified_name, n.name, '
                 'n.start_line, n.end_line ORDER BY n.qualified_name')
        session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'}, label)
    query = ("MATCH (n:CallSite) WHERE n.callee_name ENDS WITH 'response_of' "
             'RETURN n.id, n.callee_name, n.line, n.is_resolved ORDER BY n.id')
    session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'}, 'response-callsites')
    for source in ('Function', 'Method'):
        for dest in ('Function', 'Method'):
            edge = f'Calls_{source}_{dest}'
            query = (f'MATCH (a)-[r:{edge}]->(b) RETURN a.qualified_name, '
                     'b.qualified_name ORDER BY a.qualified_name, b.qualified_name')
            session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'}, edge)
    session.call('search_codebase', {**base, 'query': 'response_of', 'limit': 10}, 'search')


def extras(session, graph):
    """Appended after every hashed measurement, so the hash is unaffected."""
    base = {'graph_path': graph}
    query = ("MATCH (n:CallSite) WHERE n.callee_name ENDS WITH 'response_of' "
             'RETURN n.id, n.callee_name, n.line, n.is_resolved, '
             'n.unresolved_reason ORDER BY n.id')
    session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'},
                  'response-callsites-reason')
    query = ("MATCH (a)-[r:Calls_Method_Method]->(b) WHERE b.name = 'response_of' "
             'RETURN a.qualified_name, b.qualified_name, r.resolution_method, '
             'r.confidence ORDER BY a.qualified_name')
    session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'},
                  'response-edges-method')
    query = ("MATCH (a)-[r:Calls_Function_Method]->(b) WHERE b.name = 'response_of' "
             'RETURN a.qualified_name, b.qualified_name, r.resolution_method, '
             'r.confidence ORDER BY a.qualified_name')
    session.pages('query_graph', {**base, 'query': query, 'format': 'tabular'},
                  'response-edges-function')


def verify_corpus(corpus):
    oracle = json.loads(pathlib.Path(__file__).with_name('oracle.json').read_text())
    revision = subprocess.check_output(['git', '-C', corpus, 'rev-parse', 'HEAD'], text=True).strip()
    if revision != oracle['corpus']['commit']:
        raise RuntimeError('Corpus commit differs from source oracle')
    for item in oracle['corpus']['files']:
        actual = hashlib.sha256((pathlib.Path(corpus) / item['file']).read_bytes()).hexdigest()
        if actual != item['sha256']:
            raise RuntimeError('Corpus source differs from oracle: ' + item['file'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--corpus', required=True)
    parser.add_argument('--analyze-path', default=None)
    parser.add_argument('--output', required=True)
    parser.add_argument('--lsp', action='store_true')
    parser.add_argument('--lsp-tool', action='store_true')
    args = parser.parse_args()
    verify_corpus(args.corpus)
    directory = pathlib.Path(args.output).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    binary = str(pathlib.Path(args.binary).resolve())
    analyzed = str(pathlib.Path(args.analyze_path or args.corpus).resolve())
    identity = {'binary': binary, 'sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
                'corpus': str(pathlib.Path(args.corpus).resolve()), 'analyzed': analyzed,
                'lsp': args.lsp}
    (directory / 'identity.json').write_text(json.dumps(identity, indent=2))
    session = Session(binary, directory)
    try:
        session.call('health_check', {}, 'health')
        session.call('analyze_codebase', {'path': analyzed, 'output_dir': str(directory),
                     'language': 'rust', 'dependency_scope': 'none', 'lsp': args.lsp}, 'analyze')
        measurements(session, str(directory / 'graph'))
        extras(session, str(directory / 'graph'))
        if args.lsp_tool:
            session.send('lsp_resolve', {'graph_path': str(directory / 'graph'),
                                         'codebase_path': analyzed, 'language': 'rust'},
                         'lsp-resolve-tool')
    finally:
        session.close()


if __name__ == '__main__':
    main()
