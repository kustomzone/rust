use super::ItemCtxt;
use crate::astconv::AstConv;
use rustc_hir as hir;
use rustc_infer::traits::util;
use rustc_middle::ty::subst::InternalSubsts;
use rustc_middle::ty::{self, ImplTraitInTraitData, Ty, TyCtxt};
use rustc_span::def_id::DefId;
use rustc_span::Span;

/// For associated types we include both bounds written on the type
/// (`type X: Trait`) and predicates from the trait: `where Self::X: Trait`.
///
/// Note that this filtering is done with the items identity substs to
/// simplify checking that these bounds are met in impls. This means that
/// a bound such as `for<'b> <Self as X<'b>>::U: Clone` can't be used, as in
/// `hr-associated-type-bound-1.rs`.
fn associated_type_bounds<'tcx>(
    tcx: TyCtxt<'tcx>,
    assoc_item_def_id: DefId,
    ast_bounds: &'tcx [hir::GenericBound<'tcx>],
    span: Span,
) -> &'tcx [(ty::Predicate<'tcx>, Span)] {
    let item_ty = tcx.mk_projection(
        assoc_item_def_id,
        InternalSubsts::identity_for_item(tcx, assoc_item_def_id),
    );

    let icx = ItemCtxt::new(tcx, assoc_item_def_id);
    let mut bounds = icx.astconv().compute_bounds(item_ty, ast_bounds);
    // Associated types are implicitly sized unless a `?Sized` bound is found
    icx.astconv().add_implicitly_sized(&mut bounds, item_ty, ast_bounds, None, span);

    let trait_def_id = tcx.parent(assoc_item_def_id);
    let trait_predicates = tcx.trait_explicit_predicates_and_bounds(trait_def_id.expect_local());

    let bounds_from_parent = trait_predicates.predicates.iter().copied().filter(|(pred, _)| {
        match pred.kind().skip_binder() {
            ty::PredicateKind::Clause(ty::Clause::Trait(tr)) => tr.self_ty() == item_ty,
            ty::PredicateKind::Clause(ty::Clause::Projection(proj)) => {
                proj.projection_ty.self_ty() == item_ty
            }
            ty::PredicateKind::Clause(ty::Clause::TypeOutlives(outlives)) => outlives.0 == item_ty,
            _ => false,
        }
    });

    let all_bounds = tcx.arena.alloc_from_iter(bounds.predicates().chain(bounds_from_parent));
    debug!("associated_type_bounds({}) = {:?}", tcx.def_path_str(assoc_item_def_id), all_bounds);
    all_bounds
}

/// Opaque types don't inherit bounds from their parent: for return position
/// impl trait it isn't possible to write a suitable predicate on the
/// containing function and for type-alias impl trait we don't have a backwards
/// compatibility issue.
#[instrument(level = "trace", skip(tcx), ret)]
fn opaque_type_bounds<'tcx>(
    tcx: TyCtxt<'tcx>,
    opaque_def_id: DefId,
    ast_bounds: &'tcx [hir::GenericBound<'tcx>],
    item_ty: Ty<'tcx>,
    span: Span,
) -> &'tcx [(ty::Predicate<'tcx>, Span)] {
    ty::print::with_no_queries!({
        let icx = ItemCtxt::new(tcx, opaque_def_id);
        let mut bounds = icx.astconv().compute_bounds(item_ty, ast_bounds);
        // Opaque types are implicitly sized unless a `?Sized` bound is found
        icx.astconv().add_implicitly_sized(&mut bounds, item_ty, ast_bounds, None, span);
        debug!(?bounds);

        tcx.arena.alloc_from_iter(bounds.predicates())
    })
}

pub(super) fn explicit_item_bounds(
    tcx: TyCtxt<'_>,
    def_id: DefId,
) -> &'_ [(ty::Predicate<'_>, Span)] {
    // If the def_id is about an RPITIT, delegate explicit_item_bounds to the opaque_def_id that
    // generated the synthesized associate type.
    let rpitit_info = if let Some(ImplTraitInTraitData::Trait { opaque_def_id, .. }) =
        tcx.opt_rpitit_info(def_id)
    {
        Some(opaque_def_id)
    } else {
        None
    };

    let bounds_def_id = rpitit_info.unwrap_or(def_id);
    let hir_id = tcx.hir().local_def_id_to_hir_id(bounds_def_id.expect_local());
    match tcx.hir().get(hir_id) {
        hir::Node::TraitItem(hir::TraitItem {
            kind: hir::TraitItemKind::Type(bounds, _),
            span,
            ..
        }) => associated_type_bounds(tcx, def_id, bounds, *span),
        hir::Node::Item(hir::Item {
            kind: hir::ItemKind::OpaqueTy(hir::OpaqueTy { bounds, in_trait, .. }),
            span,
            ..
        }) => {
            let substs = InternalSubsts::identity_for_item(tcx, def_id);
            let item_ty = if *in_trait || rpitit_info.is_some() {
                tcx.mk_projection(def_id, substs)
            } else {
                tcx.mk_opaque(def_id, substs)
            };
            opaque_type_bounds(tcx, bounds_def_id, bounds, item_ty, *span)
        }
        _ => bug!("item_bounds called on {:?}", def_id),
    }
}

pub(super) fn item_bounds(
    tcx: TyCtxt<'_>,
    def_id: DefId,
) -> ty::EarlyBinder<&'_ ty::List<ty::Predicate<'_>>> {
    let bounds = tcx.mk_predicates_from_iter(
        util::elaborate_predicates(
            tcx,
            tcx.explicit_item_bounds(def_id).iter().map(|&(bound, _span)| bound),
        )
        .map(|obligation| obligation.predicate),
    );
    ty::EarlyBinder(bounds)
}
