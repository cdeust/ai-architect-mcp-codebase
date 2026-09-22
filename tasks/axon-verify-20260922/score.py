#!/usr/bin/env python3
"""Compare saved graph rows with the independently recorded source oracle.

Adapted from tasks/axon-rerun-20260908/score.py. The `semantic` dictionary and
every counter feeding it keep the 2026-09-08 definition exactly, so
`semantic_sha256` stays directly comparable. Added alongside: `lsp_status`,
`lsp_resolve`, the structured impact counts of lots 2/5
(`unresolved_callsites_naming_target`, `unresolved_callsites_outside_targets`),
the Kani-caller subset, and the resolution_method census of the
`response_of` in-edges.

Precision = TP/(TP+FP); recall = TP/(TP+FN). Undefined precision stays null.
Counters retain two source calls on one line; graph identity uses qualified
names and declaration lines. This evaluates only the declared corpus scope.
"""
import argparse
from collections import Counter
import hashlib
import json
import pathlib

LABELS = ('Function', 'Method')
EDGE_TAGS = ('response-edges-method', 'response-edges-function')
SUMMARY_KEYS = ('tp', 'fp', 'fn', 'recall')


def rows(directory, tag):
    pages = json.loads((directory / (tag + '-pages.json')).read_text())
    return [row for page in pages for row in page['rows']]


def payload(directory, tag):
    record = json.loads((directory / (tag + '.json')).read_text())
    return json.loads(record['response']['result']['content'][0]['text'])


def optional_payload(directory, tag):
    if not (directory / (tag + '.json')).exists():
        return None
    return payload(directory, tag)


def compare(expected, actual):
    want, got = Counter(expected), Counter(actual)
    tp = sum((want & got).values())
    fp, fn = sum((got - want).values()), sum((want - got).values())
    return {'tp': tp, 'fp': fp, 'fn': fn,
            'precision': tp / (tp + fp) if tp + fp else None,
            'recall': tp / (tp + fn) if tp + fn else None,
            'missing': list((want - got).elements()),
            'unexpected': list((got - want).elements())}


def all_edges(directory):
    tags = [f'Calls_{a}_{b}' for a in LABELS for b in LABELS]
    return sorted(tuple(r) for tag in tags for r in rows(directory, tag))


def definition_keys(definitions, attribute=None):
    chosen = [f for f in definitions
              if attribute is None or attribute in f['attributes']]
    return [(f['file'] + '::' + f['qualified_name'], f['line']) for f in chosen]


def callers_under(oracle, by_id, prefix):
    names = (by_id[c] for c in oracle['response_of_callers'])
    return {name for name in names if name.startswith(prefix)}


def site_key(row):
    return (row[0].split('::')[0], int(row[2]))


def semantic_of(directory, found_callers):
    actual = [(r[0], int(r[2])) for label in LABELS for r in rows(directory, label)]
    sites = rows(directory, 'response-callsites')
    return {'definitions': sorted(actual), 'edges': all_edges(directory),
            'response_callsites': sorted(sites),
            'impact_callers': sorted(found_callers)}


def census(directory):
    edge_rows = [r for tag in EDGE_TAGS for r in rows(directory, tag)]
    reason_rows = rows(directory, 'response-callsites-reason')
    return {'resolution_methods': dict(Counter(r[2] for r in edge_rows)),
            'callsite_unresolved_reasons':
                dict(Counter(r[4] for r in reason_rows if r[3] != 'true'))}


def accuracy(directory, oracle, found_callers):
    definitions = oracle['functions']
    by_id = {f['id']: f['file'] + '::' + f['qualified_name'] for f in definitions}
    actual = [(r[0], int(r[2])) for label in LABELS for r in rows(directory, label)]
    sites = rows(directory, 'response-callsites')
    expected_sites = [(c['file'], c['line']) for c in oracle['response_of_callsites']]
    resolved = [site_key(r) for r in sites if r[3] == 'true']
    found = set(found_callers)
    production = callers_under(oracle, by_id, 'src/lib.rs::TaskSet::')
    kani = callers_under(oracle, by_id, 'kani/response_bounds.rs::')
    return {'definitions': compare(definition_keys(definitions), actual),
            'test_definitions_found':
                len(set(definition_keys(definitions, '#[test]')) & set(actual)),
            'proof_definitions_found':
                len(set(definition_keys(definitions, '#[kani::proof]')) & set(actual)),
            'response_callsite_extraction':
                compare(expected_sites, [site_key(r) for r in sites]),
            'response_callsite_resolution': compare(expected_sites, resolved),
            'response_callers':
                compare([by_id[c] for c in oracle['response_of_callers']], found_callers),
            'production_callers': compare(production, found & production),
            'kani_callers': compare(kani, found & kani)}


def tool_surface(directory, impact_first):
    analyze = payload(directory, 'analyze')
    wall = json.loads((directory / 'analyze.json').read_text())['wall_seconds']
    return {'epistemic': impact_first.get('epistemic'),
            'epistemic_reasons': impact_first.get('epistemic_reasons'),
            'unresolved_callsites_naming_target':
                impact_first.get('unresolved_callsites_naming_target'),
            'unresolved_callsites_outside_targets':
                impact_first.get('unresolved_callsites_outside_targets'),
            'lsp_status': analyze.get('lsp_status'),
            'lsp_resolve': analyze.get('lsp_resolve'),
            'lsp_resolve_tool': optional_payload(directory, 'lsp-resolve-tool'),
            'coverage': payload(directory, 'coverage'), 'analyze': analyze,
            'analyze_wall_seconds': wall}


def score(directory, oracle):
    impact_pages = json.loads((directory / 'impact-pages.json').read_text())
    found_callers = [c['qualified_name'] for page in impact_pages for c in page['callers']]
    semantic = semantic_of(directory, found_callers)
    canonical = json.dumps(semantic, sort_keys=True, separators=(',', ':')).encode()
    result = {'directory': str(directory)}
    result.update(accuracy(directory, oracle, found_callers))
    result['self_edges'] = [e for e in semantic['edges'] if e[0] == e[1]]
    result['semantic_sha256'] = hashlib.sha256(canonical).hexdigest()
    result.update(census(directory))
    result.update(tool_surface(directory, impact_pages[0]))
    (directory / 'semantic.json').write_text(json.dumps(semantic, indent=2))
    return result


def summary(result):
    callers = {k: result['response_callers'][k] for k in SUMMARY_KEYS}
    return (f"{result['directory']} definitions {result['definitions']['tp']} "
            f"callers {callers} hash {result['semantic_sha256']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--oracle', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('runs', nargs='+')
    args = parser.parse_args()
    oracle = json.loads(pathlib.Path(args.oracle).read_text())
    results = [score(pathlib.Path(d), oracle) for d in args.runs]
    pathlib.Path(args.output).write_text(json.dumps(results, indent=2))
    for result in results:
        print(summary(result))


if __name__ == '__main__':
    main()
