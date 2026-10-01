#!/usr/bin/env python3
"""Drives one fresh-database MCP session (static pass) and records every reply under OUT.
Promoted from tasks/axon-main-b4714a8-20260930/measure.py with only what compare.py reads:
the LSP options, the --analyze-path probe, processes/search/coverage and the extras were dropped
(none of them feeds the compared values). Protocol/schema source: tasks/ copy, itself from the verifier.
usage: measure.py BINARY CORPUS_DIR OUT_DIR   (OUT_DIR must not exist)"""
import json
import pathlib
import subprocess
import sys
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

    def call(self, name, arguments, tag):
        """Record the request/response pair as TAG.json; return the unpacked payload."""
        self.serial += 1
        request = {'jsonrpc': '2.0', 'id': self.serial, 'method': 'tools/call',
                   'params': {'name': name, 'arguments': arguments}}
        start = time.perf_counter()
        self.process.stdin.write(json.dumps(request) + '\n')
        self.process.stdin.flush()
        reply = json.loads(self.process.stdout.readline())
        record = {'request': request, 'response': reply,
                  'wall_seconds': time.perf_counter() - start}
        (self.directory / (tag + '.json')).write_text(json.dumps(record, indent=2))
        return unpack(reply)

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


def main():
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    corpus = str(pathlib.Path(sys.argv[2]).resolve())
    directory = pathlib.Path(sys.argv[3]).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    session = Session(binary, directory)
    try:
        session.call('health_check', {}, 'health')
        session.call('analyze_codebase', {'path': corpus, 'output_dir': str(directory),
                     'language': 'rust', 'dependency_scope': 'none', 'lsp': False}, 'analyze')
        measurements(session, str(directory / 'graph'))
    finally:
        session.close()


if __name__ == '__main__':
    main()
