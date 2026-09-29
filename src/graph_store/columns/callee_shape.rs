// graph_store::columns::callee_shape: the `CallSite.callee_shape` values
// (issue #401).
//
// The C and C++ parsers record what the callee of a call is. '' is every other
// language's site and any site of a graph indexed before the column existed;
// the resolver reads it as a name.
//
// source: parser::spec::c_family writes it, resolver::calls reads it.

/// The callee is a name (`f`, `ns::f`, `f<T>`).
pub(crate) const CALLEE_SHAPE_DIRECT: &str = "direct";
/// The callee is a member access (`s.f`, `p->f`). In C it is a function
/// pointer held by a struct, since C has no methods.
pub(crate) const CALLEE_SHAPE_MEMBER: &str = "member";
/// The callee is an expression (`(*fp)`, `table[i]`, `make()`): the called
/// function is only known at run time.
pub(crate) const CALLEE_SHAPE_INDIRECT: &str = "indirect";

/// True when no static resolver can name the function a site calls: an
/// indirect callee in any language, a member access in C.
pub(crate) fn calls_through_a_pointer(language: &str, shape: &str) -> bool {
    shape == CALLEE_SHAPE_INDIRECT || (language == "c" && shape == CALLEE_SHAPE_MEMBER)
}
