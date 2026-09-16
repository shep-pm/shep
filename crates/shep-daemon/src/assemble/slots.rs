/// Finds the `count` lowest-free instance slot numbers from an existing set.
///
/// Used to allocate instance slots for clustered apps. Assumes `existing` is
/// sorted; returns a new vector of `count` distinct slots, smallest first,
/// none of which appear in `existing`.
///
/// # Examples
///
/// ```
/// use shep_daemon::assemble::instance_slots;
///
/// assert_eq!(instance_slots(&[], 3), vec![0, 1, 2]);
/// assert_eq!(instance_slots(&[0, 2], 2), vec![1, 3]);
/// ```
#[must_use]
pub fn instance_slots(existing: &[u32], count: u32) -> Vec<u32> {
    let mut result = Vec::with_capacity(count as usize);
    let mut candidate = 0u32;

    for _ in 0..count {
        while existing.contains(&candidate) || result.contains(&candidate) {
            candidate += 1;
        }
        result.push(candidate);
        candidate += 1;
    }

    result
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn slots_empty_request() {
        let result = instance_slots(&[], 3);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn slots_skip_occupied() {
        let result = instance_slots(&[0, 2], 2);
        assert_eq!(result, vec![1, 3]);
    }
}
